# Project: RustyDroid

## Architecture
RustyDroid is a lightweight hybrid Android runtime for Linux Wayland environments written in pure Rust. It enables running real Android APK applications without root privileges, heavy system daemons, or systemd services.

The system is decomposed into four modular layers:
1. **APK & Desktop Integration Layer (`src/apk`)**: Pure Rust Android Binary XML (AXML) chunk parser, APK ZIP asset extractor (icons, metadata, permissions, launcher activity/alias), and XDG Desktop Entry generator with Wayland `StartupWMClass` binding.
2. **Container & Binderfs Layer (`src/container`, `src/binder`)**: Rootless container runtime using unprivileged Linux namespaces (`CLONE_NEWUSER`, `CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWIPC`, `CLONE_NEWUTS`), UID/GID mapping with `/proc/self/setgroups = deny`, dynamic binderfs mounting (supporting both `"binder"` and `"binderfs"` kernel names), and dynamic device node creation (`binder`, `hwbinder`, `vndbinder`) via ioctl `BINDER_CTL_ADD` (`0xc1086201`).
3. **Graphics, GPU & Audio Passthrough Layer (`src/container/mounts.rs`, `src/container/namespaces.rs`)**: Direct unprivileged DRM render node passthrough (`/dev/dri/renderD128`), Wayland compositor socket forwarding (`WAYLAND_DISPLAY`) for multi-window display, and PipeWire / PulseAudio socket passthrough for audio.
4. **Lifecycle & CLI Management Layer (`src/runtime`, `src/main.rs`)**: CLI entrypoints (`info`, `install`, `run`, `init`, `status`, `stop`, `shell`), background session management (`session.json`), child subreaper (`PR_SET_CHILD_SUBREAPER`), graceful shutdown protocol, and system health diagnostics.

---

## Feature Inventory
Every feature identified during the Survey phase is mapped to a milestone below:

| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| 1 | AXML Header Validation | Validates 8-byte AXML file header magic (`0x00080003`) and chunk size | M1 | Survey |
| 2 | String Pool UTF-16 Decoding | Decodes UTF-16LE string table with multi-word continuation | M1 | Survey |
| 3 | String Pool UTF-8 Decoding | Decodes UTF-8 string table with 1- and 2-byte length prefixes | M1 | Survey |
| 4 | Resource Map & Namespace Handling | Safely parses or skips resource maps, start/end namespaces | M1 | Survey |
| 5 | Element & Attribute Decoding | Parses XML start/end tags and typed attributes (int, hex, bool, ref, string) | M1 | Survey |
| 6 | Manifest Metadata Extraction | Extracts package name, versionCode, versionName, permissions | M1 | Survey |
| 7 | Launcher Activity Resolution | Locates activity/activity-alias with `action.MAIN` + `category.LAUNCHER` | M1 | Survey |
| 8 | Density Icon Discovery | Prioritized extraction from `mipmap` / `drawable` + fallback search | M1 | Survey |
| 9 | XDG Desktop Entry Generation | Creates standard `.desktop` file with `StartupWMClass` in applications dir | M1 | Survey |
| 10 | Icon Theme Installation | Saves extracted PNG/WebP to XDG hicolor icon directories | M1 | Survey |
| 11 | Unprivileged Namespaces Setup | `CLONE_NEWUSER`, `CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWIPC`, `CLONE_NEWUTS` | M2 | Survey |
| 12 | UID/GID Rootless Mapping | Writes `deny` to `/proc/self/setgroups`, maps root UID 0 to host UID | M2 | Survey |
| 13 | Dynamic Binderfs Mount | Dynamically mounts binder filesystem (handling `"binder"` / `"binderfs"`) | M2 | Survey |
| 14 | Binder Nodes Allocation | Dynamically creates `binder`, `hwbinder`, `vndbinder` via `BINDER_CTL_ADD` | M2 | Survey |
| 15 | Rootless Container FS Layout | Bubblewrap layout with `--tmpfs /` preventing read-only mount collisions | M2 | Survey |
| 16 | Direct GPU Node Passthrough | Verifies `/dev/dri/renderD128` and configures unprivileged container bind | M3 | Survey |
| 17 | Wayland Socket Passthrough | Forwards `$WAYLAND_DISPLAY` UNIX socket into container environment | M3 | Survey |
| 18 | PipeWire / PulseAudio Routing | Forwards audio sockets (`pipewire-0`, `pulse/native`) into container | M3 | Survey |
| 19 | CLI `info` Subcommand | Autonomous APK inspection without Java/AAPT | M4 | Survey |
| 20 | CLI `install` Subcommand | Desktop entry and icon registration | M4 | Survey |
| 21 | CLI `status` Subcommand | Comprehensive system health and runtime diagnostics | M4 | Survey |
| 22 | CLI `init` Subcommand | Prepares storage tree, tests binderfs, checks images | M4 | Survey |
| 23 | CLI `run` Subcommand | Starts container session if needed, sends launch intent | M4 | Survey |
| 24 | CLI `stop` Subcommand | Graceful shutdown (SIGTERM -> poll -> SIGKILL) and cleanup | M4 | Survey |
| 25 | CLI `shell` Subcommand | Runs interactive or batch command in container | M4 | Survey |
| 26 | Session State & Child Subreaper | `session.json` tracking, stale session purge, `PR_SET_CHILD_SUBREAPER` | M4 | Survey |
| 27 | Full E2E Test Suite (Tiers 1-4) | Comprehensive opaque-box test suite passing 100% | M5 | Survey |
| 28 | Adversarial Coverage Hardening | White-box stress testing and gap elimination (Tier 5) | M5 | Survey |

---

## Milestones

| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| M1 | `apk_desktop` | APK Binary XML Parsing, Asset Extraction & XDG Desktop Integration (Features 1-10) | none | DONE |
| M2 | `binderfs_runtime` | Rootless Container Runtime, Namespaces & Binderfs Orchestration (Features 11-15) | none | DONE |
| M3 | `wayland_gpu_audio` | Wayland Multi-Window, Direct GPU Passthrough & PipeWire Audio (Features 16-18) | M2 | DONE |
| M4 | `lifecycle_cli` | Hybrid Application Lifecycle & CLI Management (Features 19-26) | M1, M2, M3 | DONE |
| M5 | `e2e_acceptance` | Final Milestone: 100% E2E Test Pass (Tiers 1-4) & Adversarial Hardening (Tier 5) (Features 27-28) | M4, TEST_READY.md | DONE |

In parallel:
- **`e2e_testing_track`**: E2E Testing Track Orchestrator designing opaque-box test runner and test cases (Tiers 1-4) publishing `TEST_READY.md`.

---

## Interface Contracts

### M1 (`src/apk`) ↔ M4 (`src/main.rs`, `src/runtime`)
- `ApkInspector::inspect(path: &Path) -> Result<ApkMetadata, ApkError>`
  - Returns `ApkMetadata` struct: `package_name: String`, `version_code: Option<u32>`, `version_name: Option<String>`, `label: Option<String>`, `main_activity: Option<String>`, `icon_data: Option<Vec<u8>>`, `icon_path: Option<String>`, `permissions: Vec<String>`.
- `DesktopIntegration::install_desktop_entry(metadata: &ApkMetadata, binary_name: &str) -> Result<PathBuf, DesktopError>`
  - Installs `.desktop` file to `~/.local/share/applications/rustydroid-<package>.desktop`.
  - Installs icon to `~/.local/share/icons/hicolor/192x192/apps/rustydroid-<package>.png`.

### M2 (`src/binder`, `src/container`) ↔ M3 (`src/container/mounts.rs`) & M4 (`src/runtime/session.rs`)
- `BinderManager::is_supported() -> bool`
  - Detects if kernel binderfs is available (checks `/proc/filesystems` for `"binder"` or `"binderfs"`).
- `BinderManager::mount_binderfs(target: &Path) -> Result<(), BinderError>`
  - Mounts binderfs inside unprivileged user/mount namespace with dynamic fallback between `"binder"` and `"binderfs"`.
- `BinderManager::setup_standard_devices(target: &Path) -> Result<Vec<PathBuf>, BinderError>`
  - Allocates `binder`, `hwbinder`, `vndbinder` devices via `BINDER_CTL_ADD` ioctl (`0xc1086201`) and sets permissions to `0o666`.
- `NamespaceManager::build_bwrap_command(config: &ContainerConfig) -> Result<Command, ContainerError>`
  - Constructs `bwrap` command using `--tmpfs /` and ephemeral directory creation (`--dir /data`, `--dir /dev/binderfs`, etc.) to prevent read-only root collisions.

### M3 (`src/container/mounts.rs`) ↔ M4 (`src/runtime/session.rs`, `src/main.rs`)
- `verify_gpu_access() -> Result<PathBuf, ContainerError>`
  - Validates accessibility of `/dev/dri/renderD128` without root.
- `get_wayland_socket() -> Option<PathBuf>`
  - Resolves `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` socket.
- `get_pulse_socket() -> Option<PathBuf>`
  - Resolves `$XDG_RUNTIME_DIR/pulse/native` or PipeWire socket.

### M4 (`src/runtime/session.rs`) ↔ Sentinel & CLI
- `SessionManager::start_session() -> Result<SessionState, SessionError>`
  - Spawns supervisor daemon, records PID in `session.json`, mounts binderfs inside container userns.
- `SessionManager::stop_session() -> Result<(), SessionError>`
  - Sends SIGTERM, waits up to 2 seconds, sends SIGKILL if still running, purges `session.json`.
- `SessionManager::get_status() -> SessionStatus`
  - Checks process liveness and cleans stale `session.json` if deceased.

---

## Code Layout

- `src/`
  - `lib.rs`: Public module exports (`apk`, `binder`, `container`, `runtime`, `adb`).
  - `main.rs`: CLI binary parsing commands and dispatching to subsystem managers.
  - `apk/`
    - `mod.rs`: APK submodule declarations and error types.
    - `axml.rs`: Pure Rust Android Binary XML parser.
    - `inspector.rs`: APK ZIP extraction and metadata inspection.
    - `desktop.rs`: XDG Desktop Entry and icon installation/removal.
  - `binder/`
    - `mod.rs`: Binderfs mount and device management.
    - `ioctl.rs`: Kernel `BINDER_CTL_ADD` ioctl structure and syscall wrappers.
  - `container/`
    - `mod.rs`: Container configuration and types.
    - `namespaces.rs`: Linux namespace creation and Bubblewrap sandbox builder.
    - `mounts.rs`: Storage layout, GPU DRI verification, Wayland/Audio socket discovery.
  - `runtime/`
    - `mod.rs`: Runtime module declarations.
    - `session.rs`: Session supervisor daemon, PID tracking, subreaper, and shutdown.
    - `config.rs`: Runtime paths (`~/.local/share/rustydroid`, `$XDG_RUNTIME_DIR/rustydroid`).
    - `images.rs`: Android image metadata and status checking.
  - `adb/`
    - `mod.rs`, `client.rs`: Lightweight ADB client for sending launch intents and shell commands.
- `tests/`
  - `apk_parser_test.rs`: Unit & integration tests for AXML and APK parsing.
  - `binderfs_test.rs`: Tests for binderfs support, constants, and device creation.
  - `container_test.rs`: Tests for namespace configuration, bwrap arguments, and GPU verification.
  - `e2e/`: Opaque-box E2E test suite (created by E2E Testing Track).
