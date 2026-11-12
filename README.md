# RustyDroid

> **Disambiguation:** This project (`Kirieshka14/rustydroid`) is a Linux/Wayland CLI utility and compatibility wrapper around Waydroid for installing and running Android `.apk`, `.xapk`, `.apks`, and `.apkm` packages on desktop Linux. It has **no relation or affiliation** with the Android RAT / malware known under the name "RustyDroid".

RustyDroid is a Rust CLI tool for inspecting, installing, and launching Android packages on Linux Wayland compositors via Waydroid. It automates split-APK bundle extraction, OBB asset staging, ABI mismatch detection, ADB session setup, per-app window sizing, and optional ARM/ARM64 translation setup.

## Purpose

Running Android packages directly on desktop Linux typically requires manual handling of split bundles (`.xapk`, `.apks`, `.apkm`), missing `libndk_translation` libraries on x86_64 hosts, ADB authorization prompts, Waydroid multi-window properties, and compositor window geometry. RustyDroid consolidates these steps into a single CLI workflow:

- **Package inspection:** Parses binary `AndroidManifest.xml` (AXML) metadata, extracts launcher activities and icons, and analyzes native ABIs without invoking external Java tooling.
- **Bundle & OBB handling:** Unpacks `.xapk`, `.apks`, and `.apkm` archives, selects splits matching host/container ABIs, stages OBB directories, and installs split APKs via `adb install-multiple`.
- **Container & runtime diagnostics:** Detects Waydroid runner state, container ABIs, `ro.hardware.gralloc` mismatches (`gbm` vs `minigbm_gbm_mesa`), and translates ADB / `pm install` errors into actionable output.
- **Per-app adaptation profiles:** Stores window mode (`auto`, `freeform`, `single`, `portrait`, `landscape`, `custom`), dimensions, DPI, `fake_wifi`, `fake_touch`, installer spoofing, and SDK compatibility flags per package.
- **Hyprland window synchronization:** When running under Hyprland, queries `hyprctl` and listens read-only to Hyprland's `.socket2.sock` to keep Android task bounds synchronized with tiled/floating window sizes without modifying user Hyprland or Waybar configuration files.
- **Virtual keyboard control:** Keeps Android's on-screen IME disabled by default so the Waydroid input-method surface does not trigger unwanted compositor window splits; controllable globally or per launch.

## Requirements

- Linux x86_64 or aarch64 with binder/ashmem kernel support (`binder_linux` / `binderfs`)
- Wayland compositor (tested on Hyprland)
- `waydroid` and Android system/vendor images
- `android-tools` (`adb`)
- `python`, `curl`, `unzip` (used by ARM translation setup script)
- Optional: `libnotify` (`notify-send`), `bubblewrap`

## Installation

### Arch Linux (AUR)

```bash
yay -S rustydroid
# or
paru -S rustydroid
```

### Local Arch Package (`makepkg`)

```bash
git clone https://github.com/Kirieshka14/rustydroid.git
cd rustydroid/packaging
makepkg -si
```

### Build from Source (Cargo)

Requires Rust toolchain (`cargo` / `rustc`):

```bash
git clone https://github.com/Kirieshka14/rustydroid.git
cd rustydroid
cargo build --release

install -Dm755 target/release/rustydroid ~/.local/bin/rustydroid
install -Dm755 packaging/install-waydroid-runner.sh ~/.local/bin/install-waydroid-runner.sh
install -Dm755 packaging/install-waydroid-arm-translation.sh ~/.local/bin/install-waydroid-arm-translation.sh
```

Optional desktop integration (MIME types, `.desktop` launcher, Dolphin service menu):

```bash
install -Dm644 packaging/rustydroid.desktop ~/.local/share/applications/rustydroid.desktop
install -Dm644 packaging/rustydroid-android-bundles.xml ~/.local/share/mime/packages/rustydroid-android-bundles.xml
install -Dm644 packaging/rustydroid-servicemenu.desktop ~/.local/share/kio/servicemenus/rustydroid.desktop
update-desktop-database ~/.local/share/applications 2>/dev/null || true
update-mime-database ~/.local/share/mime 2>/dev/null || true
```

## Initial Waydroid Setup

1. Install and initialize the Waydroid runner (requires `sudo`):

   ```bash
   rustydroid setup-runner
   ```

2. If Waydroid Android images are not yet present in `/var/lib/waydroid/images` or `/usr/share/waydroid-extra/images`, initialize them:

   ```bash
   sudo waydroid init
   ```

3. On x86_64 hosts, install ARM/ARM64 native bridge translation (`libndk_translation`):

   ```bash
   rustydroid setup-arm
   ```

4. Verify host and container readiness:

   ```bash
   rustydroid status
   ```

## Usage

### Run or Install Packages

```bash
# Inspect APK or split bundle metadata and ABI compatibility
rustydroid info app.apk

# Install and launch an APK, XAPK, APKS, or APKM file
rustydroid run app.xapk

# Launch an already installed Android package by package name
rustydroid run com.example.app

# Run in fixed phone portrait window mode
rustydroid run --window-mode portrait app.apk

# Install APK and register an XDG desktop menu shortcut (.desktop + icon)
rustydroid install app.apk
```

### Per-App Adaptation Profiles

```bash
rustydroid adapt show com.example.app
rustydroid adapt set com.example.app --window-mode tablet --fake-wifi --fake-touch
rustydroid adapt list
rustydroid adapt reset com.example.app
```

Common adaptation flags for `run`, `install`, and `adapt set`:

| Flag | Description |
| --- | --- |
| `--window-mode <MODE>` | `auto`, `freeform`, `single`, `portrait`, `landscape`, `custom` |
| `--width <PX>` / `--height <PX>` | Override Android task width/height in pixels |
| `--dpi <DPI>` | Override virtual LCD density |
| `--fake-wifi` / `--no-fake-wifi` | Toggle `persist.waydroid.fake_wifi` |
| `--fake-touch` / `--no-fake-touch` | Toggle `persist.waydroid.fake_touch` |
| `--abi <ABI>` | Force split/native ABI during installation (e.g. `arm64-v8a`) |
| `--installer <PKG>` / `--no-spoof` | Set or disable installer package spoofing |
| `--force-sdk` | Enable low-target-SDK installation compatibility (`--force-sdk`) |
| `--allow-downgrade` | Pass `-d` during package installation |
| `--no-auto-grant` | Do not auto-grant runtime permissions (`-g`) |
| `--ime` / `--no-ime` | Enable or disable Android IME for a run/profile |
| `--save-profile` | Persist flags passed to `run` / `install` as the app's profile |

### Virtual Keyboard (IME)

Android IME is disabled by default:

```bash
rustydroid keyboard status
rustydroid keyboard off
rustydroid keyboard on
```

### Session Control

```bash
rustydroid status
rustydroid shell
rustydroid stop
```

## License

Released under the [MIT License](LICENSE).
