#![allow(dead_code, unused_imports)]

use crate::apk::compat::{classify_pm_install_failure, AdaptationProfile};
use anyhow::{bail, Context, Result};
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub replace: bool,
    pub allow_test: bool,
    pub allow_downgrade: bool,
    pub grant_permissions: bool,
    pub installer_pkg: Option<String>,
    pub abi: Option<String>,
    pub force_sdk: bool,
    pub bypass_low_target_sdk_block: bool,
}

impl InstallOptions {
    pub fn from_profile(profile: &AdaptationProfile) -> Self {
        Self {
            replace: true,
            allow_test: profile.allow_test_only,
            allow_downgrade: profile.allow_downgrade,
            grant_permissions: profile.auto_grant_permissions,
            installer_pkg: profile.installer_spoof.clone(),
            abi: profile.abi_override.clone(),
            force_sdk: profile.force_sdk,
            bypass_low_target_sdk_block: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub used_fallback: Option<String>,
    pub warnings: Vec<String>,
}

pub struct AdbClient;

impl AdbClient {
    /// Checks if adb binary is available on the host system
    pub fn is_adb_available() -> bool {
        Command::new("adb")
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Checks if an Android device or container adb daemon is connected
    pub fn is_device_connected() -> bool {
        Command::new("adb")
            .args(&["get-state"])
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "device")
            .unwrap_or(false)
    }

    /// Installs an APK into the running container
    pub fn install_apk(apk_path: &str) -> Result<String> {
        let output = Command::new("adb")
            .args(&["install", "-r", apk_path])
            .output()
            .context("Failed to execute adb install")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            bail!("adb install failed:\nSTDOUT: {}\nSTDERR: {}", stdout, stderr);
        }

        Ok(stdout)
    }

    /// Launches the main activity for a package
    pub fn start_activity(package_name: &str, activity_name: &str) -> Result<String> {
        let target = if activity_name.starts_with('.') {
            format!("{}/{}", package_name, activity_name)
        } else if !activity_name.contains('/') {
            format!("{}/{}", package_name, activity_name)
        } else {
            activity_name.to_string()
        };

        let output = Command::new("adb")
            .args(&["shell", "am", "start", "-n", &target])
            .output()
            .context("Failed to execute adb shell am start")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            bail!("am start failed: {}\n{}", stdout, stderr);
        }

        Ok(stdout)
    }

    /// Stops an application
    pub fn stop_app(package_name: &str) -> Result<()> {
        let _ = Command::new("adb")
            .args(&["shell", "am", "force-stop", package_name])
            .status();
        Ok(())
    }

    /// Lists installed third-party packages
    pub fn list_packages() -> Result<Vec<String>> {
        let output = Command::new("adb")
            .args(&["shell", "pm", "list", "packages", "-3"])
            .output()
            .context("Failed to execute pm list packages")?;

        let text = String::from_utf8_lossy(&output.stdout);
        let mut packages = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(pkg) = line.strip_prefix("package:") {
                packages.push(pkg.to_string());
            }
        }
        Ok(packages)
    }

    /// Executes arbitrary shell command inside container
    pub fn shell_command(cmd_args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("adb");
        cmd.arg("shell");
        for a in cmd_args {
            cmd.arg(a);
        }
        let output = cmd.output().context("Failed to execute adb shell command")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Unfreezes a frozen Waydroid container over system D-Bus before ADB calls so adbd responds immediately.
    pub fn unfreeze_waydroid_container() -> bool {
        let tools: &[(&str, &[&str])] = &[
            (
                "gdbus",
                &[
                    "call",
                    "--system",
                    "--dest",
                    "id.waydro.Container",
                    "--object-path",
                    "/ContainerManager",
                    "--method",
                    "id.waydro.ContainerManager.Unfreeze",
                ],
            ),
            (
                "busctl",
                &[
                    "call",
                    "--system",
                    "id.waydro.Container",
                    "/ContainerManager",
                    "id.waydro.ContainerManager",
                    "Unfreeze",
                ],
            ),
            (
                "dbus-send",
                &[
                    "--system",
                    "--print-reply",
                    "--dest=id.waydro.Container",
                    "/ContainerManager",
                    "id.waydro.ContainerManager.Unfreeze",
                ],
            ),
        ];

        for (bin, args) in tools {
            if let Ok(status) = Command::new(bin)
                .args(*args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                if status.success() {
                    return true;
                }
            }
        }
        false
    }

    /// Detects the active Waydroid container IP address from `waydroid status`, falling back to `192.168.240.112`.
    pub fn detect_waydroid_ip() -> String {
        if let Ok(out) = Command::new("waydroid").arg("status").output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("IP address:") {
                    let ip = rest.trim();
                    if !ip.is_empty() && ip != "UNKNOWN" {
                        return ip.to_string();
                    }
                }
            }
        }
        "192.168.240.112".to_string()
    }

    /// Resolves host Waydroid data directory (`$XDG_DATA_HOME/waydroid/data` or `~/.local/share/waydroid/data`).
    pub fn waydroid_data_dir() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("waydroid/data");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".local/share/waydroid/data")
    }

    /// Authorizes the host `~/.android/adbkey.pub` inside Waydroid `/data/misc/adb/adb_keys` rootlessly.
    pub fn ensure_waydroid_adb_authorized(waydroid_data_dir: &Path) -> Result<()> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let pubkey_path = PathBuf::from(home).join(".android/adbkey.pub");
        if !pubkey_path.exists() {
            let _ = Command::new("adb")
                .arg("start-server")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let Ok(pubkey_raw) = fs::read_to_string(&pubkey_path) else {
            return Ok(());
        };
        let pubkey_line = pubkey_raw.trim();
        if pubkey_line.is_empty() {
            return Ok(());
        }

        let adb_dir = waydroid_data_dir.join("misc/adb");
        if adb_dir.exists() {
            let keys_file = adb_dir.join("adb_keys");
            let existing = fs::read_to_string(&keys_file).unwrap_or_default();
            if !existing.lines().any(|l| l.trim() == pubkey_line) {
                let mut updated = existing;
                if !updated.is_empty() && !updated.ends_with('\n') {
                    updated.push('\n');
                }
                updated.push_str(pubkey_line);
                updated.push('\n');
                fs::write(&keys_file, updated)
                    .with_context(|| format!("Не удалось записать {}", keys_file.display()))?;
                let _ = fs::set_permissions(&keys_file, Permissions::from_mode(0o640));
            }
        }
        Ok(())
    }

    fn run_adb_with_timeout(args: &[&str], timeout_secs: u32) -> Result<Output> {
        let secs = timeout_secs.to_string();
        let timeout_res = Command::new("timeout")
            .arg(&secs)
            .arg("adb")
            .args(args)
            .stdin(Stdio::null())
            .output();
        match timeout_res {
            Ok(out) => Ok(out),
            Err(_) => Command::new("adb")
                .args(args)
                .stdin(Stdio::null())
                .output()
                .context("Failed to execute adb"),
        }
    }

    /// Connects ADB cleanly to the target Waydroid `<ip>:5555` endpoint without touching physical USB devices.
    pub fn connect_target(serial: &str) -> Result<()> {
        if !Self::is_adb_available() {
            bail!("adb не найден в PATH");
        }
        Self::unfreeze_waydroid_container();
        let _ = Self::ensure_waydroid_adb_authorized(&Self::waydroid_data_dir());

        if let Ok(out) = Self::run_adb_with_timeout(&["-s", serial, "get-state"], 3) {
            if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "device" {
                return Ok(());
            }
        }

        let _ = Self::run_adb_with_timeout(&["disconnect", serial], 2);
        let conn = Self::run_adb_with_timeout(&["connect", serial], 5)?;
        let conn_text = format!(
            "{}{}",
            String::from_utf8_lossy(&conn.stdout),
            String::from_utf8_lossy(&conn.stderr)
        );
        let state = Self::run_adb_with_timeout(&["-s", serial, "get-state"], 4)?;
        let state_str = String::from_utf8_lossy(&state.stdout).trim().to_string();
        if state.status.success() && state_str == "device" {
            return Ok(());
        }
        bail!(
            "ADB не удалось подключиться к {} (state='{}', details='{}')",
            serial,
            state_str,
            conn_text.trim()
        )
    }

    /// Builds `adb install` or `adb install-multiple` argument vector uniformly.
    pub fn build_adb_install_args(
        serial: &str,
        apk_paths: &[PathBuf],
        opts: &InstallOptions,
    ) -> Vec<String> {
        let mut args = vec!["-s".to_string(), serial.to_string()];
        if apk_paths.len() > 1 {
            args.push("install-multiple".to_string());
        } else {
            args.push("install".to_string());
        }
        if opts.replace {
            args.push("-r".to_string());
        }
        if opts.allow_test {
            args.push("-t".to_string());
        }
        if opts.allow_downgrade {
            args.push("-d".to_string());
        }
        if opts.grant_permissions {
            args.push("-g".to_string());
        }
        if let Some(ref installer) = opts.installer_pkg {
            if !installer.trim().is_empty() {
                args.push("-i".to_string());
                args.push(installer.trim().to_string());
            }
        }
        if let Some(ref abi) = opts.abi {
            if !abi.trim().is_empty() {
                args.push("--abi".to_string());
                args.push(abi.trim().to_string());
            }
        }
        if opts.bypass_low_target_sdk_block {
            args.push("--bypass-low-target-sdk-block".to_string());
        } else if opts.force_sdk {
            args.push("--force-sdk".to_string());
        }
        for path in apk_paths {
            args.push(path.to_string_lossy().to_string());
        }
        args
    }

    /// Installs one or multiple (split) APKs via targeted ADB with automatic recovery retries.
    pub fn install_with_adaptation(
        serial: &str,
        apk_paths: &[PathBuf],
        base_opts: &InstallOptions,
    ) -> Result<InstallOutcome> {
        if apk_paths.is_empty() {
            bail!("Нет APK файлов для установки");
        }
        Self::unfreeze_waydroid_container();

        let mut opts = base_opts.clone();
        let mut fallback_notes = Vec::new();
        let mut warnings = Vec::new();

        for attempt in 0..3 {
            let args = Self::build_adb_install_args(serial, apk_paths, &opts);
            let out = Command::new("adb")
                .args(&args)
                .stdin(Stdio::null())
                .output()
                .context("Не удалось выполнить adb install")?;

            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if out.status.success() && !combined.contains("Failure [") {
                return Ok(InstallOutcome {
                    used_fallback: if fallback_notes.is_empty() {
                        None
                    } else {
                        Some(fallback_notes.join(", "))
                    },
                    warnings,
                });
            }

            let diag = classify_pm_install_failure(&combined);
            if attempt < 2 {
                if diag.retry_with_downgrade && !opts.allow_downgrade {
                    opts.allow_downgrade = true;
                    fallback_notes.push("downgrade(-d)".to_string());
                    continue;
                }
                if diag.retry_with_force_sdk && !opts.force_sdk && !opts.bypass_low_target_sdk_block {
                    opts.force_sdk = true;
                    fallback_notes.push("force-sdk".to_string());
                    continue;
                }
                if diag.retry_with_bypass_low_sdk && !opts.bypass_low_target_sdk_block {
                    opts.force_sdk = false;
                    opts.bypass_low_target_sdk_block = true;
                    fallback_notes.push("bypass-low-target-sdk-block".to_string());
                    continue;
                }
                if diag.retry_without_grant && opts.grant_permissions {
                    opts.grant_permissions = false;
                    fallback_notes.push("without-g".to_string());
                    warnings.push(diag.summary_ru);
                    continue;
                }
                // Some adb/Android builds reject `--force-sdk` or `-i` syntax on `install` -> fallback cleanly
                if (combined.contains("Unknown option --force-sdk")
                    || combined.contains("Unknown option --bypass-low-target-sdk-block"))
                    && (opts.force_sdk || opts.bypass_low_target_sdk_block)
                {
                    if opts.force_sdk {
                        opts.force_sdk = false;
                        opts.bypass_low_target_sdk_block = true;
                    } else {
                        opts.bypass_low_target_sdk_block = false;
                    }
                    continue;
                }
            }

            bail!("{}", diag.summary_ru);
        }

        bail!("Не удалось установить пакет после повторных попыток")
    }

    /// Uninstalls a package from the targeted ADB device/container cleanly.
    pub fn uninstall_target(serial: &str, package_name: &str) -> Result<()> {
        Self::unfreeze_waydroid_container();
        let out = Self::run_adb_with_timeout(&["-s", serial, "uninstall", package_name], 30)?;
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() && !combined.contains("Unknown package") {
            bail!("Ошибка удаления {}: {}", package_name, combined.trim());
        }
        Ok(())
    }

    /// Pushes `.obb` expansion files cleanly to `/sdcard/Android/obb/<pkg>/`.
    pub fn push_obb_files(serial: &str, obb_mappings: &[(PathBuf, String)]) -> Result<()> {
        Self::unfreeze_waydroid_container();
        for (host_file, remote_path) in obb_mappings {
            if let Some(parent) = Path::new(remote_path).parent() {
                let parent_str = parent.to_string_lossy();
                let _ = Self::run_adb_with_timeout(
                    &["-s", serial, "shell", "mkdir", "-p", &parent_str],
                    10,
                );
            }
            let host_str = host_file.to_string_lossy().to_string();
            let out = Self::run_adb_with_timeout(
                &["-s", serial, "push", &host_str, remote_path],
                120,
            )?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                bail!("Ошибка копирования OBB {}: {}", remote_path, err.trim());
            }
        }
        Ok(())
    }

    pub fn shell_for(serial: &str, cmd_args: &[&str]) -> Result<String> {
        Self::unfreeze_waydroid_container();
        let mut full_args = vec!["-s", serial, "shell"];
        full_args.extend_from_slice(cmd_args);
        let out = Self::run_adb_with_timeout(&full_args, 10)?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn get_prop(serial: &str, key: &str) -> Option<String> {
        let out = Self::shell_for(serial, &["getprop", key]).ok()?;
        let trimmed = out.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Reads current container display bounds (`waydroid.display_width`/`height` or `wm size`).
    pub fn get_display_bounds(serial: &str) -> (u32, u32) {
        let w = Self::get_prop(serial, "waydroid.display_width")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&v| v >= 240);
        let h = Self::get_prop(serial, "waydroid.display_height")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&v| v >= 240);
        if let (Some(w), Some(h)) = (w, h) {
            return (w, h);
        }
        if let Ok(wm_out) = Self::shell_for(serial, &["wm", "size"]) {
            for line in wm_out.lines() {
                if let Some(dim_str) = line.split(':').nth(1) {
                    if let Some((rw, rh)) = dim_str.trim().split_once('x') {
                        if let (Ok(pw), Ok(ph)) = (rw.parse::<u32>(), rh.parse::<u32>()) {
                            return (pw, ph);
                        }
                    }
                }
            }
        }
        (1280, 720)
    }

    /// Applies or resets live `wm size` and `wm density` overrides in the running container.
    pub fn apply_wm_geometry(
        serial: &str,
        dims: Option<(u32, u32)>,
        density: Option<u32>,
    ) -> Result<()> {
        match dims {
            Some((w, h)) => {
                let dim_arg = format!("{}x{}", w, h);
                let _ = Self::shell_for(serial, &["wm", "size", &dim_arg])?;
            }
            None => {
                if Self::shell_for(serial, &["wm", "size"])
                    .map(|out| out.contains("Override size:"))
                    .unwrap_or(false)
                {
                    let _ = Self::shell_for(serial, &["wm", "size", "reset"]);
                }
            }
        }
        match density {
            Some(dpi) => {
                let dpi_arg = dpi.to_string();
                let _ = Self::shell_for(serial, &["wm", "density", &dpi_arg])?;
            }
            None => {
                if Self::shell_for(serial, &["wm", "density"])
                    .map(|out| out.contains("Override density:"))
                    .unwrap_or(false)
                {
                    let _ = Self::shell_for(serial, &["wm", "density", "reset"]);
                }
            }
        }
        Ok(())
    }

    /// Launches a package via Android `am start` or `monkey` fallback if Waydroid D-Bus session is unavailable.
    pub fn launch_app_via_adb(serial: &str, package: &str, activity: Option<&str>) -> Result<()> {
        Self::unfreeze_waydroid_container();
        if let Some(act) = activity {
            let component = if act.contains('/') {
                act.to_string()
            } else if act.starts_with('.') {
                format!("{}/{}", package, act)
            } else {
                format!("{}/{}", package, act)
            };
            let out = Self::shell_for(serial, &["am", "start", "-n", &component])?;
            if !out.contains("Error:") {
                return Ok(());
            }
        }
        let out = Self::shell_for(
            serial,
            &[
                "monkey",
                "-p",
                package,
                "-c",
                "android.intent.category.LAUNCHER",
                "1",
            ],
        )?;
        if out.contains("No activities found") {
            bail!("Не найдена LAUNCHER Activity для пакета {}", package);
        }
        Ok(())
    }
}
