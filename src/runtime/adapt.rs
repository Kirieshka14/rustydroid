#![allow(dead_code)]

use crate::adb::client::InstallOptions;
use crate::adb::AdbClient;
use crate::apk::compat::{
    container_supports_arm, resolve_window_dimensions, select_install_splits,
    stage_bundle_artifacts, AbiStatus, AdaptationProfile, ApkBundleAnalyzer, BundleFormat,
    BundleInspection, ProfileOverrides, ProfileStore, SplitKind, WindowMode,
};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Default)]
pub struct AdaptLaunchOptions {
    pub overrides: ProfileOverrides,
    pub save_profile: bool,
    pub dry_run_install_only: bool,
}

/// Checks whether Waydroid output lines indicate a CLI/session error.
pub fn waydroid_text_has_error(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    upper.contains("ERROR:")
        || upper.contains("TRACEBACK")
        || upper.contains("WAYDROID SESSION IS STOPPED")
        || upper.contains("FAILED TO ACCESS IPLATFORM")
        || upper.contains("NOT INITIALIZED")
        || upper.contains("NOT FOUND")
        || upper.contains("FAILED TO INSTALL")
        || upper.contains("INSTALLATION FAILED")
}

/// Detects container-supported ABIs from runtime props, `waydroid_base.prop`, or libndk_translation overlay.
pub fn detect_container_abis() -> Vec<String> {
    if let Some(raw) = waydroid_prop_get_quiet("ro.product.cpu.abilist") {
        let parsed = parse_abi_csv(&raw);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    detect_container_abis_from_paths(
        Some(Path::new("/var/lib/waydroid/waydroid_base.prop")),
        Some(Path::new("/var/lib/waydroid/overlay/system/lib64/libndk_translation.so")),
    )
}

pub fn detect_container_abis_from_paths(
    base_prop_path: Option<&Path>,
    overlay_ndk_so: Option<&Path>,
) -> Vec<String> {
    if let Some(prop_path) = base_prop_path {
        if let Ok(content) = fs::read_to_string(prop_path) {
            for line in content.lines() {
                if let Some(val) = line.trim().strip_prefix("ro.product.cpu.abilist=") {
                    let parsed = parse_abi_csv(val);
                    if !parsed.is_empty() {
                        return parsed;
                    }
                }
            }
        }
    }
    if let Some(so_path) = overlay_ndk_so {
        if so_path.exists() {
            return vec![
                "x86_64".to_string(),
                "x86".to_string(),
                "arm64-v8a".to_string(),
                "armeabi-v7a".to_string(),
                "armeabi".to_string(),
            ];
        }
    }
    vec!["x86_64".to_string(), "x86".to_string()]
}

fn parse_abi_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Reads an Android property cleanly via `waydroid prop get`, returning `None` if session isn't active or empty.
pub fn waydroid_prop_get_quiet(key: &str) -> Option<String> {
    let out = Command::new("waydroid")
        .args(["prop", "get", key])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if waydroid_text_has_error(&stdout) || waydroid_text_has_error(&stderr) {
        return None;
    }
    let val = stdout.trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Sets a Waydroid property using `waydroid prop set` with `adb shell setprop` fallback.
pub fn sync_waydroid_prop(serial: Option<&str>, key: &str, value: &str) {
    let out = Command::new("waydroid")
        .args(["prop", "set", key, value])
        .stdin(Stdio::null())
        .output();
    let ok = out
        .as_ref()
        .map(|o| {
            o.status.success()
                && !waydroid_text_has_error(&String::from_utf8_lossy(&o.stderr))
                && !waydroid_text_has_error(&String::from_utf8_lossy(&o.stdout))
        })
        .unwrap_or(false);

    if !ok {
        if let Some(ser) = serial {
            let _ = AdbClient::shell_for(ser, &["setprop", key, value]);
        }
    }
}

/// Applies per-app adaptation profile properties (`fake_wifi`, `fake_touch`, `multi_windows`, `wm size`/`density`).
pub fn apply_runtime_profile(serial: Option<&str>, profile: &AdaptationProfile) {
    let pkg = &profile.package_name;

    // 1. Update fake_wifi CSV property cleanly within 91-char Android property limit
    let existing_wifi = waydroid_prop_get_quiet("persist.waydroid.fake_wifi")
        .or_else(|| serial.and_then(|s| AdbClient::get_prop(s, "persist.waydroid.fake_wifi")))
        .unwrap_or_default();
    let updated_wifi = crate::apk::compat::merge_prop_package_list(
        &existing_wifi,
        pkg,
        profile.fake_wifi,
        91,
    );
    if updated_wifi != existing_wifi {
        sync_waydroid_prop(serial, "persist.waydroid.fake_wifi", &updated_wifi);
    }

    // 2. Update fake_touch CSV property cleanly within 91-char Android property limit
    let existing_touch = waydroid_prop_get_quiet("persist.waydroid.fake_touch")
        .or_else(|| serial.and_then(|s| AdbClient::get_prop(s, "persist.waydroid.fake_touch")))
        .unwrap_or_default();
    let updated_touch = crate::apk::compat::merge_prop_package_list(
        &existing_touch,
        pkg,
        profile.fake_touch,
        91,
    );
    if updated_touch != existing_touch {
        sync_waydroid_prop(serial, "persist.waydroid.fake_touch", &updated_touch);
    }

    // 3. Window integration hints
    match profile.window_mode {
        WindowMode::Freeform => {
            sync_waydroid_prop(serial, "persist.waydroid.multi_windows", "true");
        }
        WindowMode::SingleWindow => {
            sync_waydroid_prop(serial, "persist.waydroid.multi_windows", "false");
        }
        _ => {}
    }

    // 4. Custom props defined in profile
    for (k, v) in &profile.extra_props {
        sync_waydroid_prop(serial, k, v);
    }

    // 5. Live window resolution / density scaling clamped cleanly to host display
    if let Some(ser) = serial {
        let (host_w, host_h) = AdbClient::get_display_bounds(ser);
        let dims = resolve_window_dimensions(profile, host_w, host_h);
        if let Err(e) = AdbClient::apply_wm_geometry(ser, dims, profile.lcd_density) {
            eprintln!("⚠️  Не удалось применить wm size/density: {}", e);
        } else if let Some((w, h)) = dims {
            println!("🪟 Адаптация размера экрана Android: {}x{} (экран Waydroid: {}x{})", w, h, host_w, host_h);
        }
    }
}

/// Installs an `.apk` / `.xapk` / `.apks` / `.apkm` package using the adaptation pipeline.
pub fn install_bundle_adapted(
    inspection: &BundleInspection,
    profile: &AdaptationProfile,
    serial: Option<&str>,
) -> Result<()> {
    let container_abis = detect_container_abis();
    if let AbiStatus::ArmTranslationRequired {
        ref abis,
        container_supports_arm: false,
    } = inspection.compat.abi_status
    {
        println!(
            "⚠️  Внимание: пакет требует ARM ABI ({}), а трансляция ARM в контейнере не активна.",
            abis.join(", ")
        );
        println!("👉 Выполните `rustydroid setup-arm`, если установка или запуск завершится ошибкой ABI.");
    }

    let selected_splits = select_install_splits(
        inspection,
        &container_abis,
        profile.abi_override.as_deref(),
        None,
    );

    let clean_pkg: String = inspection
        .metadata
        .package_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c } else { '_' })
        .collect();
    let staging_dir = AdbClient::waydroid_data_dir()
        .join("waydroid_tmp")
        .join(format!("rustydroid_{}", clean_pkg));

    let plan = stage_bundle_artifacts(inspection, &selected_splits, &staging_dir)?;
    if inspection.compat.format != BundleFormat::SingleApk {
        println!(
            "📦 Распакован {} ({} APK часть/ей, {} OBB)",
            inspection.compat.format.display_label(),
            plan.apk_paths.len(),
            plan.obb_files.len()
        );
    }

    if let Some(ser) = serial {
        let mut opts = InstallOptions::from_profile(profile);
        // For single APK without forced ABI, pick best matching ABI if container supports ARM and apk is ARM-only
        if opts.abi.is_none() && container_supports_arm(&container_abis) {
            if let AbiStatus::ArmTranslationRequired { ref abis, .. } = inspection.compat.abi_status {
                if abis.iter().any(|a| a == "arm64-v8a")
                    && container_abis.iter().any(|c| c == "arm64-v8a")
                {
                    opts.abi = Some("arm64-v8a".to_string());
                } else if abis.iter().any(|a| a == "armeabi-v7a")
                    && container_abis.iter().any(|c| c == "armeabi-v7a")
                {
                    opts.abi = Some("armeabi-v7a".to_string());
                }
            }
        }

        let install_res = AdbClient::install_with_adaptation(ser, &plan.apk_paths, &opts);
        match install_res {
            Ok(outcome) => {
                finish_adb_install(ser, outcome, inspection, &plan)?;
                return Ok(());
            }
            Err(adb_err) => {
                let err_msg = adb_err.to_string();
                let is_sig_conflict = err_msg.contains("Конфликт цифровой подписи");
                if is_sig_conflict {
                    if prompt_signature_reinstall(&inspection.metadata.package_name) {
                        println!(
                            "🗑️  Удаляем старую версию {} (конфликт подписи)...",
                            inspection.metadata.package_name
                        );
                        AdbClient::uninstall_target(ser, &inspection.metadata.package_name)?;
                        let retry_outcome =
                            AdbClient::install_with_adaptation(ser, &plan.apk_paths, &opts)?;
                        finish_adb_install(ser, retry_outcome, inspection, &plan)?;
                        return Ok(());
                    }
                    cleanup_staging_dir(&inspection.compat.format, &plan.staging_dir);
                    bail!(
                        "Приложение {} уже установлено в Waydroid с другой цифровой подписью.\n\
                         👉 Удалите старую версию (данные приложения будут удалены): adb -s {} uninstall {}\n\
                         👉 Затем запустите снова: rustydroid run \"{}\"",
                        inspection.metadata.package_name,
                        ser,
                        inspection.metadata.package_name,
                        inspection.source_path.display()
                    );
                }

                let is_definitive_pm_rejection = err_msg.contains("Конфликт")
                    || err_msg.contains("Несовместимость")
                    || err_msg.contains("minSdkVersion")
                    || err_msg.contains("targetSdkVersion")
                    || err_msg.contains("Недостаточно памяти")
                    || err_msg.contains("Некорректный или неподписанный")
                    || err_msg.contains("Уже установлена более новая");
                if inspection.compat.format == BundleFormat::SingleApk && !is_definitive_pm_rejection {
                    eprintln!(
                        "⚠️  ADB install вернул ошибку ({}), пробуем waydroid app install...",
                        adb_err
                    );
                    fallback_waydroid_install(&inspection.source_path)?;
                    cleanup_staging_dir(&inspection.compat.format, &plan.staging_dir);
                    return Ok(());
                }
                cleanup_staging_dir(&inspection.compat.format, &plan.staging_dir);
                return Err(adb_err);
            }
        }
    } else if inspection.compat.format == BundleFormat::SingleApk {
        fallback_waydroid_install(&inspection.source_path)?;
        return Ok(());
    } else {
        cleanup_staging_dir(&inspection.compat.format, &plan.staging_dir);
        bail!(
            "Установка split-пакета {} требует активного ADB соединения с Waydroid (проверьте `adb` и состояние контейнера).",
            inspection.compat.format.display_label()
        );
    }
}

fn finish_adb_install(
    serial: &str,
    outcome: crate::adb::client::InstallOutcome,
    inspection: &BundleInspection,
    plan: &crate::apk::compat::StagingPlan,
) -> Result<()> {
    if let Some(fb) = outcome.used_fallback {
        println!("🛠️  Применён авто-адаптер установки: {}", fb);
    }
    for w in outcome.warnings {
        println!("⚠️  {}", w);
    }
    if !plan.obb_files.is_empty() {
        println!(
            "📂 Копирование {} OBB кэш-файлов в /sdcard/Android/obb/...",
            plan.obb_files.len()
        );
        AdbClient::push_obb_files(serial, &plan.obb_files)?;
    }
    cleanup_staging_dir(&inspection.compat.format, &plan.staging_dir);
    Ok(())
}

fn prompt_signature_reinstall(package_name: &str) -> bool {
    use std::io::{self, IsTerminal, Write};
    if !io::stdin().is_terminal() {
        return false;
    }
    eprint!(
        "⚠️  Пакет {} уже установлен с другой цифровой подписью.\n\
         Удалить старую версию (её данные сотрутся) и установить новую? [y/N]: ",
        package_name
    );
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let ans = line.trim().to_ascii_lowercase();
    matches!(ans.as_str(), "y" | "yes" | "д" | "да")
}

fn cleanup_staging_dir(format: &BundleFormat, staging_dir: &Path) {
    if *format != BundleFormat::SingleApk && staging_dir.exists() {
        let _ = fs::remove_dir_all(staging_dir);
    }
}

fn fallback_waydroid_install(apk_path: &Path) -> Result<()> {
    let out = Command::new("waydroid")
        .args(["app", "install", &apk_path.to_string_lossy()])
        .output()
        .context("Failed to execute waydroid app install")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() || waydroid_text_has_error(&stdout) || waydroid_text_has_error(&stderr) {
        bail!(
            "Не удалось установить APK через Waydroid.\n{}\n{}",
            stdout.trim(),
            stderr.trim()
        );
    }
    Ok(())
}

/// Parses `key=val` properties from an Android `.prop` file.
pub fn parse_prop_value(content: &str, key: &str) -> Option<String> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Parses `key = val` inside a given `[section]` of an INI file (`waydroid.cfg`).
pub fn parse_ini_property(content: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let current = line[1..line.len() - 1].trim();
            in_section = current == section;
            continue;
        }
        if in_section {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Inserts or updates a property under `[properties]` in `waydroid.cfg` text.
pub fn ensure_cfg_property(cfg_content: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = cfg_content.lines().map(ToString::to_string).collect();
    let mut in_props = false;
    let mut props_header_idx = None;
    let mut existing_idx = None;
    let mut section_end_idx = lines.len();

    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_props {
                section_end_idx = idx;
                break;
            }
            if trimmed[1..trimmed.len() - 1].trim() == "properties" {
                in_props = true;
                props_header_idx = Some(idx);
            }
            continue;
        }
        if in_props && !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with(';') {
            if let Some((k, _)) = trimmed.split_once('=') {
                if k.trim() == key {
                    existing_idx = Some(idx);
                    break;
                }
            }
        }
    }

    let formatted = format!("{} = {}", key, value);
    if let Some(idx) = existing_idx {
        lines[idx] = formatted;
    } else if props_header_idx.is_some() {
        lines.insert(section_end_idx, formatted);
    } else {
        if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        lines.push("[properties]".to_string());
        lines.push(formatted);
    }

    let mut out = lines.join("\n");
    if cfg_content.ends_with('\n') || !out.is_empty() {
        out.push('\n');
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrallocDiagnostic {
    pub base_prop_gralloc: Option<String>,
    pub cfg_prop_gralloc: Option<String>,
    pub runtime_gralloc: Option<String>,
    pub legacy_gbm_enabled: bool,
    pub minigbm_allocator_running: bool,
}

impl GrallocDiagnostic {
    /// True when Waydroid passes `ro.hardware.gralloc=gbm` to an image using `minigbm_gbm_mesa`,
    /// causing `hwcomposer` / compositor `error 5: x/y < 1` crashes on Hyprland/Wayland.
    pub fn is_mismatch(&self) -> bool {
        if self.legacy_gbm_enabled {
            return false;
        }
        let active_is_gbm = self.runtime_gralloc.as_deref() == Some("gbm")
            || self.base_prop_gralloc.as_deref() == Some("gbm");
        let cfg_fixed = self.cfg_prop_gralloc.as_deref() == Some("minigbm_gbm_mesa");
        active_is_gbm && (!cfg_fixed || self.runtime_gralloc.as_deref() == Some("gbm")) && self.minigbm_allocator_running
    }

    pub fn remediation_message(&self) -> String {
        "Обнаружено несоответствие графического аллокатора Waydroid: `ro.hardware.gralloc=gbm` при активном `minigbm_gbm_mesa` вызывает краш окна на Hyprland (x/y < 1).\n👉 Добавьте `ro.hardware.gralloc = minigbm_gbm_mesa` в секцию `[properties]` файла `/var/lib/waydroid/waydroid.cfg` и выполните:\n     sudo waydroid upgrade -o && sudo systemctl restart waydroid-container".to_string()
    }
}

pub fn inspect_gralloc_from_sources(
    base_prop: Option<&str>,
    cfg_content: Option<&str>,
    runtime_gralloc: Option<&str>,
    minigbm_allocator_running: bool,
) -> GrallocDiagnostic {
    let base_prop_gralloc = base_prop.and_then(|c| parse_prop_value(c, "ro.hardware.gralloc"));
    let legacy_gbm_enabled = base_prop
        .and_then(|c| parse_prop_value(c, "gralloc.gbm.legacy"))
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let cfg_prop_gralloc = cfg_content.and_then(|c| parse_ini_property(c, "properties", "ro.hardware.gralloc"));

    GrallocDiagnostic {
        base_prop_gralloc,
        cfg_prop_gralloc,
        runtime_gralloc: runtime_gralloc.map(ToString::to_string),
        legacy_gbm_enabled,
        minigbm_allocator_running,
    }
}

fn is_proc_binary_running(binary_suffix: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv0 = raw.split(|&b| b == 0).next().unwrap_or(&[]);
        if let Ok(cmd) = std::str::from_utf8(argv0) {
            if cmd == binary_suffix || cmd.ends_with(binary_suffix) {
                return true;
            }
        }
    }
    false
}

/// Checks `/var/lib/waydroid/{waydroid_base.prop,waydroid.cfg}` and runtime properties for gralloc mismatch.
pub fn detect_waydroid_gralloc_diagnostic(include_runtime_prop: bool) -> GrallocDiagnostic {
    let base_prop = fs::read_to_string("/var/lib/waydroid/waydroid_base.prop").ok();
    let cfg_content = fs::read_to_string("/var/lib/waydroid/waydroid.cfg").ok();
    let runtime_gralloc = if include_runtime_prop {
        waydroid_prop_get_quiet("ro.hardware.gralloc")
    } else {
        None
    };
    let minigbm_running =
        is_proc_binary_running("/android.hardware.graphics.allocator@4.0-service.minigbm_gbm_mesa");

    inspect_gralloc_from_sources(
        base_prop.as_deref(),
        cfg_content.as_deref(),
        runtime_gralloc.as_deref(),
        minigbm_running,
    )
}

/// Evaluates whether Android has finished booting and User 0 storage is unlocked cleanly.
pub fn parse_boot_readiness(boot_completed: Option<&str>, ce_available: Option<&str>) -> bool {
    if boot_completed.map(str::trim) != Some("1") {
        return false;
    }
    !matches!(ce_available.map(str::trim), Some("false") | Some("0"))
}

/// Returns true when Waydroid's `IPlatform` service responds with `sys.boot_completed=1`.
pub fn is_android_user_ready() -> bool {
    let boot = waydroid_prop_get_quiet("sys.boot_completed");
    if boot.as_deref().map(str::trim) != Some("1") {
        return false;
    }
    let ce = waydroid_prop_get_quiet("sys.user.0.ce_available");
    parse_boot_readiness(boot.as_deref(), ce.as_deref())
}

/// Connects to Waydroid ADB endpoint if available, returning `Some("<ip>:5555")` cleanly.
pub fn try_connect_waydroid_adb() -> Option<String> {
    try_connect_waydroid_adb_with_retry(1, 0)
}

/// Connects to Waydroid ADB endpoint with bounded retries (useful right after Android finishes booting).
pub fn try_connect_waydroid_adb_with_retry(attempts: u32, delay_ms: u64) -> Option<String> {
    if !AdbClient::is_adb_available() {
        return None;
    }
    let tries = attempts.max(1);
    for idx in 0..tries {
        AdbClient::unfreeze_waydroid_container();
        let ip = AdbClient::detect_waydroid_ip();
        let serial = format!("{}:5555", ip);
        if AdbClient::connect_target(&serial).is_ok() {
            return Some(serial);
        }
        if idx + 1 < tries && delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }
    None
}

fn is_waydroid_dbus_session_active() -> bool {
    Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            "id.waydro.Session",
        ])
        .stdin(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("(true,)"))
        .unwrap_or(false)
}

fn is_package_installed_via_adb(serial: &str, package: &str) -> Option<bool> {
    let out = AdbClient::shell_for(serial, &["pm", "path", package]).ok()?;
    if out.contains("package:") {
        Some(true)
    } else if out.trim().is_empty() {
        Some(false)
    } else {
        None
    }
}

/// Launches an installed Android app using `waydroid app launch` with ADB Activity fallback.
pub fn launch_adapted_app(
    serial: Option<&str>,
    package: &str,
    activity: Option<&str>,
    display_name: &str,
    profile: &AdaptationProfile,
) -> Result<()> {
    AdbClient::unfreeze_waydroid_container();

    if let Some(ser) = serial {
        if let Some(false) = is_package_installed_via_adb(ser, package) {
            bail!(
                "Пакет {} не установлен в контейнере Waydroid.\n👉 Установите APK командой: rustydroid run /путь/к/файлу.apk",
                package
            );
        }
    }

    apply_runtime_profile(serial, profile);

    println!("🚀 Запуск окна приложения: {} ({}) ...", display_name, package);
    if !is_waydroid_dbus_session_active() {
        if let Some(ser) = serial {
            AdbClient::launch_app_via_adb(ser, package, activity)?;
            return Ok(());
        }
    }

    AdbClient::unfreeze_waydroid_container();
    let launch_out = Command::new("waydroid")
        .args(["app", "launch", package])
        .output()
        .context("Не удалось выполнить waydroid app launch")?;
    let stdout = String::from_utf8_lossy(&launch_out.stdout);
    let stderr = String::from_utf8_lossy(&launch_out.stderr);

    if !launch_out.status.success()
        || waydroid_text_has_error(&stdout)
        || waydroid_text_has_error(&stderr)
    {
        if let Some(ser) = serial {
            eprintln!(
                "⚠️  waydroid app launch недоступен ({}), запускаем напрямую через ADB...",
                stderr.trim()
            );
            AdbClient::launch_app_via_adb(ser, package, activity)?;
            return Ok(());
        }
        bail!(
            "Не удалось запустить {} через Waydroid.\n{}\n{}",
            package,
            stdout.trim(),
            stderr.trim()
        );
    }

    Ok(())
}

/// Prints structured compatibility and adaptation details for `rustydroid info`.
pub fn print_compatibility_report(inspection: &BundleInspection, active_profile: &AdaptationProfile) {
    let c = &inspection.compat;
    println!("\n🧩 Compatibility & Adaptation Analysis:");
    println!("  • Format:        {}", c.format.display_label());
    let abis_text = if c.native_abis.is_empty() {
        "Pure Java/Kotlin DEX (universal)".to_string()
    } else {
        c.native_abis.join(", ")
    };
    let abi_note = match &c.abi_status {
        AbiStatus::PureDex => "native ART speed",
        AbiStatus::NativeX86 { .. } => "native x86/x86_64 available",
        AbiStatus::ArmTranslationRequired {
            container_supports_arm: true,
            ..
        } => "ARM translation ready (libndk_translation)",
        AbiStatus::ArmTranslationRequired {
            container_supports_arm: false,
            ..
        } => "MISSING ARM translation! Run `rustydroid setup-arm`",
    };
    println!("  • Native ABIs:   {} [{}]", abis_text, abi_note);
    println!(
        "  • SDK Versions:  min={}, target={}, compile={}",
        c.min_sdk.map(|v| v.to_string()).unwrap_or_else(|| "N/A".to_string()),
        c.target_sdk.map(|v| v.to_string()).unwrap_or_else(|| "N/A".to_string()),
        c.compile_sdk.map(|v| v.to_string()).unwrap_or_else(|| "N/A".to_string()),
    );
    println!(
        "  • Orientation:   {} (resizeable={}, game={})",
        c.orientation.display_label(),
        c.resizeable_activity
            .map(|b| b.to_string())
            .unwrap_or_else(|| "default".to_string()),
        c.is_game
    );
    if let Some(ref eng) = c.game_engine {
        println!("  • Engine:        {}", eng);
    }
    if c.uses_gms {
        println!("  • Play Services: detected ({})", c.gms_signals.join("; "));
    }
    if !c.splits.is_empty() {
        println!("  • Split APKs:    {} file(s)", c.splits.len());
    }
    if !c.obb_files.is_empty() {
        println!("  • OBB Data:      {} file(s)", c.obb_files.len());
    }

    println!("\n⚙️  Effective Adaptation Profile ({}):", active_profile.package_name);
    print_profile_fields(active_profile);

    if !c.warnings.is_empty() {
        println!("\n⚠️  Compatibility Notes:");
        for w in &c.warnings {
            println!("  - {}", w);
        }
    }
}

pub fn print_profile_fields(p: &AdaptationProfile) {
    println!("  • Window Mode:   {:?}", p.window_mode);
    if p.width.is_some() || p.height.is_some() {
        println!(
            "  • Window Size:   {}x{}",
            p.width.map(|v| v.to_string()).unwrap_or_else(|| "auto".to_string()),
            p.height.map(|v| v.to_string()).unwrap_or_else(|| "auto".to_string())
        );
    }
    if let Some(dpi) = p.lcd_density {
        println!("  • LCD Density:   {} dpi", dpi);
    }
    println!("  • fake_wifi:     {}", p.fake_wifi);
    println!("  • fake_touch:    {}", p.fake_touch);
    println!(
        "  • Installer:     {}",
        p.installer_spoof.as_deref().unwrap_or("none")
    );
    println!("  • Auto-grant:    {}", p.auto_grant_permissions);
    if let Some(ref abi) = p.abi_override {
        println!("  • ABI Override:  {}", abi);
    }
    if p.force_sdk {
        println!("  • Force SDK:     true");
    }
    if p.allow_downgrade {
        println!("  • Downgrade:     true (-d)");
    }
}

/// Finds `install-waydroid-arm-translation.sh` in user/system binary locations.
pub fn find_arm_setup_script() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let candidates = [
        PathBuf::from(home).join(".local/bin/install-waydroid-arm-translation.sh"),
        PathBuf::from("/usr/local/bin/install-waydroid-arm-translation.sh"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Resolves package name and optional `BundleInspection` cleanly from a target string (`.apk`/bundle path OR `<pkg>`).
pub fn inspect_target_or_package(
    target: &str,
    is_archive: bool,
    overrides: &ProfileOverrides,
) -> Result<(String, Option<String>, String, Option<BundleInspection>, AdaptationProfile)> {
    if is_archive {
        let container_abis = detect_container_abis();
        let inspection = ApkBundleAnalyzer::inspect(target, &container_abis)?;
        let label = inspection
            .metadata
            .app_label
            .as_deref()
            .unwrap_or(&inspection.metadata.package_name)
            .to_string();
        let profile = ProfileStore::resolve_effective(
            &inspection.metadata.package_name,
            Some(&inspection.compat),
            overrides,
        );
        let pkg = inspection.metadata.package_name.clone();
        let act = inspection.metadata.main_activity.clone();
        return Ok((pkg, act, label, Some(inspection), profile));
    }

    let profile = ProfileStore::resolve_effective(target, None, overrides);
    Ok((target.to_string(), None, target.to_string(), None, profile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_detect_container_abis_from_base_prop_and_overlay() {
        let dir = tempdir().unwrap();
        let prop_file = dir.path().join("waydroid_base.prop");
        fs::write(
            &prop_file,
            "sys.use_memfd=true\nro.product.cpu.abilist=x86_64,x86,arm64-v8a,armeabi-v7a,armeabi\n",
        )
        .unwrap();

        let abis = detect_container_abis_from_paths(Some(&prop_file), None);
        assert_eq!(
            abis,
            vec!["x86_64", "x86", "arm64-v8a", "armeabi-v7a", "armeabi"]
        );

        let missing_prop = dir.path().join("missing.prop");
        let fake_so = dir.path().join("libndk_translation.so");
        fs::write(&fake_so, b"ELF").unwrap();
        let from_so = detect_container_abis_from_paths(Some(&missing_prop), Some(&fake_so));
        assert!(container_supports_arm(&from_so));

        let x86_only = detect_container_abis_from_paths(Some(&missing_prop), Some(&dir.path().join("none.so")));
        assert!(!container_supports_arm(&x86_only));
        assert_eq!(x86_only, vec!["x86_64", "x86"]);
    }

    #[test]
    fn test_gralloc_mismatch_detection_and_cfg_update() {
        let base_gbm = "sys.use_memfd=true\nro.hardware.gralloc=gbm\n";
        let cfg_empty = "[waydroid]\narch = x86_64\n\n[properties]\nro.vndk.lite = true\n";
        let diag_mismatch =
            inspect_gralloc_from_sources(Some(base_gbm), Some(cfg_empty), Some("gbm"), true);
        assert!(diag_mismatch.is_mismatch());

        // Legacy flag suppresses mismatch warning
        let base_legacy = "ro.hardware.gralloc=gbm\ngralloc.gbm.legacy=true\n";
        let diag_legacy =
            inspect_gralloc_from_sources(Some(base_legacy), Some(cfg_empty), Some("gbm"), true);
        assert!(!diag_legacy.is_mismatch());

        // Fixing waydroid.cfg + base_prop clears mismatch
        let updated_cfg = ensure_cfg_property(cfg_empty, "ro.hardware.gralloc", "minigbm_gbm_mesa");
        assert_eq!(
            parse_ini_property(&updated_cfg, "properties", "ro.hardware.gralloc").as_deref(),
            Some("minigbm_gbm_mesa")
        );
        let base_fixed = "sys.use_memfd=true\nro.hardware.gralloc=minigbm_gbm_mesa\n";
        let diag_ok = inspect_gralloc_from_sources(
            Some(base_fixed),
            Some(&updated_cfg),
            Some("minigbm_gbm_mesa"),
            true,
        );
        assert!(!diag_ok.is_mismatch());
    }

    #[test]
    fn test_parse_boot_readiness() {
        assert!(!parse_boot_readiness(None, None));
        assert!(!parse_boot_readiness(Some("0"), Some("true")));
        assert!(!parse_boot_readiness(Some("1"), Some("false")));
        assert!(!parse_boot_readiness(Some("1"), Some("0")));
        assert!(parse_boot_readiness(Some("1"), Some("true")));
        assert!(parse_boot_readiness(Some("1\n"), None));
    }
}
