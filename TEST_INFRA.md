# TEST_INFRA.md — RustyDroid Test Infrastructure

## 1. Overview & Objectives

RustyDroid's test infrastructure provides an automated, requirement-driven, opaque-box end-to-end (E2E) testing framework. It exercises all 28 features defined in `PROJECT.md § Feature Inventory` across the entire functional stack:
- **APK & Desktop Integration Layer** (pure Rust AXML parsing, asset discovery, XDG `.desktop` generation, icon extraction)
- **Rootless Container Runtime & Binderfs Layer** (unprivileged namespaces, UID/GID mapping, binderfs dynamic mount detection, BINDER_CTL_ADD ioctl)
- **Graphics, GPU & Audio Passthrough Layer** (DRM render nodes, Wayland socket routing, PipeWire/PulseAudio discovery)
- **Lifecycle & CLI Management Layer** (subcommands `info`, `install`, `run`, `init`, `status`, `stop`, `shell`, session state tracking, and child subreaping)

All tests execute **100% offline and deterministically** without requiring external network connectivity, AAPT/Java tooling, or root privileges.

---

## 2. Directory Layout & Architecture

The test infrastructure is structured under `tests/` as follows:

```
tests/
├── common/
│   ├── mod.rs                  # Common test harness exports
│   ├── synthetic_apk.rs        # Synthetic APK and Binary XML (AXML) generator
│   └── sandbox.rs              # Hermetic sandbox environment with isolated XDG/HOME
├── e2e/
│   ├── tier1_feature_coverage.rs         # Tier 1: Happy-path tests for Features 1–26
│   ├── tier2_boundary_corner_cases.rs    # Tier 2: Truncated headers, overflows, missing files
│   ├── tier3_cross_feature_combinations.rs# Tier 3: Pairwise & multi-feature interactions
│   ├── tier4_real_world_scenarios.rs     # Tier 4: F-Droid & Aurora Store simulated workloads
│   └── tier5_adversarial_hardening.rs    # Tier 5 / Feature 28: Malicious & adversarial inputs
├── e2e_test.rs                 # Master integration test runner (`cargo test --test e2e_test`)
├── apk_parser_test.rs          # Component tests for AXML parser & inspector
├── binderfs_test.rs            # Component tests for binderfs & ioctl constants
└── container_test.rs           # Component tests for namespaces & bwrap command building
```

---

## 3. Synthetic Test Data Generation (`tests/common/synthetic_apk.rs`)

To ensure tests run independently of external APK downloads, the test suite includes pure-Rust synthetic generators:

1. **`AxmlBuilder`**:
   - Constructs valid Android Binary XML files directly in memory according to the Android AOSP chunk format.
   - Generates 8-byte file headers (`0x00080003` magic and total file size).
   - Generates dual UTF-16LE and UTF-8 string pool chunks (`0x00010001`):
     - UTF-16 mode with multi-word continuation support for lengths $\ge 0x8000$.
     - UTF-8 mode (`flags & (1 << 8) != 0`) with 1-byte ($len \le 127$) and 2-byte ($len > 127$) length prefixes.
   - Generates optional Resource Map chunks (`0x00080180`) and Start/End Namespace chunks (`0x00100100` / `0x00100101`).
   - Generates start elements (`0x00100102`) and end elements (`0x00100103`) with typed attribute conversion:
     - `0x03` (`TYPE_STRING`): string pool index lookup
     - `0x10` (`TYPE_INT_DEC`): decimal integers
     - `0x11` (`TYPE_INT_HEX`): hexadecimal integers
     - `0x12` (`TYPE_INT_BOOLEAN`): boolean values (`true`/`false`)
     - `0x01` (`TYPE_REFERENCE`): resource references (`@0x{:08x}`)

2. **`SyntheticApkBuilder`**:
   - Packages AXML manifests into valid ZIP archives.
   - Supports embedding application icons at arbitrary locations:
     - Standard density mipmaps: `res/mipmap-xxxhdpi-v4/ic_launcher.png`, `res/mipmap-hdpi-v4/ic_launcher.png`, `res/drawable/`
     - Non-standard/obfuscated assets: `assets/swap-icon.png`, `res/drawable/custom.webp`
   - Generates synthetic valid PNG byte streams (`sample_png_bytes()`) with valid PNG header magic (`\x89PNG\r\n\x1a\n`), `IHDR`, and `IEND` chunks.

---

## 4. Hermetic Test Sandbox (`tests/common/sandbox.rs`)

To prevent tests from polluting the host system or suffering from race conditions, `TestSandbox` provides process-level isolation:

- Allocates an ephemeral `tempfile::TempDir`.
- Pre-creates isolated XDG hierarchy:
  - Applications: `<sandbox>/.local/share/applications`
  - Icons: `<sandbox>/.local/share/icons/hicolor/192x192/apps`
  - Runtime Data: `<sandbox>/.local/share/rustydroid`
  - Configuration: `<sandbox>/.config/rustydroid`
  - Sockets: `<sandbox>/run`
- Sets environment variables for all CLI sub-process invocations:
  - `HOME = <sandbox>`
  - `XDG_DATA_HOME = <sandbox>/.local/share`
  - `XDG_CONFIG_HOME = <sandbox>/.config`
  - `XDG_RUNTIME_DIR = <sandbox>/run`
- Captures `stdout`, `stderr`, and `ExitStatus` for opaque-box CLI assertions.

---

## 5. Multi-Tier Test Methodology

The E2E test suite (`tests/e2e_test.rs`) is partitioned into 5 tiers:

| Tier | Name | Focus | Test Count |
|---|---|---|---|
| **Tier 1** | Feature Coverage | Representative happy-path verification for Features 1–26 | 26 |
| **Tier 2** | Boundary & Corner Cases | Truncated buffers, invalid magic, dead PIDs, missing icons, name overflows | 18 |
| **Tier 3** | Cross-Feature Combinations | Pairwise flows (Parse $\to$ Desktop $\to$ Icon, Init $\to$ Status, Install $\to$ Run) | 7 |
| **Tier 4** | Real-World Scenarios | F-Droid 2.0.2 simulation, Aurora Store simulation, full CLI lifecycle | 3 |
| **Tier 5** | Adversarial Hardening | Malformed chunks, chunk overflows, special characters, corrupted JSON | 6 |
| **Total** | | **Comprehensive E2E Suite** | **60** |

Together with component tests (`apk_parser_test.rs`: 15 tests, `binderfs_test.rs`: 2 tests, `container_test.rs`: 4 tests), the overall workspace includes **81 automated tests**.

---

## 6. How to Run Tests

### Running the E2E Test Suite
```bash
cargo test --test e2e_test
```

### Running a Specific Tier
```bash
# Run Tier 1 Feature Coverage
cargo test --test e2e_test tier1

# Run Tier 2 Boundary Cases
cargo test --test e2e_test tier2

# Run Tier 3 Combinations
cargo test --test e2e_test tier3

# Run Tier 4 Real-World Scenarios
cargo test --test e2e_test tier4

# Run Tier 5 Adversarial Tests
cargo test --test e2e_test tier5
```

### Running All Project Tests
```bash
cargo test
```

### Running with Real-Time Output
```bash
cargo test --test e2e_test -- --nocapture
```
