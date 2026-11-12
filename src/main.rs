use clap::{Parser, Subcommand};
use rustydroid::adb::AdbClient;
use rustydroid::apk::compat::{
    is_package_archive_path, ProfileOverrides, ProfileStore, WindowMode,
};
use rustydroid::apk::DesktopIntegration;
use rustydroid::binder::BinderManager;
use rustydroid::container::{ContainerConfig, MountManager, NamespaceManager};
use rustydroid::runtime::adapt::{
    detect_waydroid_gralloc_diagnostic, find_arm_setup_script, inspect_target_or_package,
    install_bundle_adapted, launch_adapted_app, print_compatibility_report, print_profile_fields,
    try_connect_waydroid_adb, try_connect_waydroid_adb_with_retry,
};
use rustydroid::runtime::{ConfigManager, ImageManager, SessionManager};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "rustydroid",
    author = "RustyDroid Contributors",
    version = "0.1.0",
    about = "Lightweight Waydroid analogue in Rust to run Android APK files on Linux/Wayland"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Args, Debug, Clone, Default)]
struct AdaptFlags {
    /// Window adaptation mode: auto, freeform, single, portrait, landscape, custom
    #[arg(long = "window-mode", value_name = "MODE")]
    window_mode: Option<String>,
    /// Override virtual Android window width (pixels)
    #[arg(long)]
    width: Option<u32>,
    /// Override virtual Android window height (pixels)
    #[arg(long)]
    height: Option<u32>,
    /// Override virtual Android LCD density (DPI)
    #[arg(long)]
    dpi: Option<u32>,
    /// Spoof Wi-Fi connectivity for this app (persist.waydroid.fake_wifi)
    #[arg(long = "fake-wifi", conflicts_with = "no_fake_wifi")]
    fake_wifi: bool,
    /// Disable fake Wi-Fi for this app
    #[arg(long = "no-fake-wifi")]
    no_fake_wifi: bool,
    /// Translate mouse clicks/drags into touch events (persist.waydroid.fake_touch)
    #[arg(long = "fake-touch", conflicts_with = "no_fake_touch")]
    fake_touch: bool,
    /// Disable fake touch for this app
    #[arg(long = "no-fake-touch")]
    no_fake_touch: bool,
    /// Force native ABI during installation (e.g. arm64-v8a, armeabi-v7a, x86_64)
    #[arg(long)]
    abi: Option<String>,
    /// Spoof installer package name (default: com.android.vending)
    #[arg(long, value_name = "PKG")]
    installer: Option<String>,
    /// Disable Play Store installer spoofing
    #[arg(long = "no-spoof")]
    no_spoof: bool,
    /// Do not auto-grant runtime permissions (-g) during installation
    #[arg(long = "no-auto-grant")]
    no_auto_grant: bool,
    /// Allow version downgrade (-d) during installation
    #[arg(long = "allow-downgrade")]
    allow_downgrade: bool,
    /// Force low-target-SDK compatibility (--force-sdk)
    #[arg(long = "force-sdk")]
    force_sdk: bool,
    /// Save these adaptation settings as a persistent per-app profile
    #[arg(long = "save-profile")]
    save_profile: bool,
}

impl AdaptFlags {
    fn to_overrides(&self) -> anyhow::Result<ProfileOverrides> {
        let window_mode = match self.window_mode.as_deref().map(str::to_ascii_lowercase).as_deref() {
            None => None,
            Some("auto") => Some(WindowMode::Auto),
            Some("freeform") | Some("multi") | Some("multiwindow") => Some(WindowMode::Freeform),
            Some("single") | Some("fullscreen") => Some(WindowMode::SingleWindow),
            Some("portrait") | Some("phone") => Some(WindowMode::PhonePortrait),
            Some("landscape") | Some("tablet") => Some(WindowMode::TabletLandscape),
            Some("custom") => Some(WindowMode::CustomSize),
            Some(other) => anyhow::bail!(
                "Неизвестный режим окна '{}'. Доступны: auto, freeform, single, portrait, landscape, custom",
                other
            ),
        };

        Ok(ProfileOverrides {
            window_mode,
            width: self.width,
            height: self.height,
            lcd_density: self.dpi,
            fake_wifi: if self.fake_wifi {
                Some(true)
            } else if self.no_fake_wifi {
                Some(false)
            } else {
                None
            },
            fake_touch: if self.fake_touch {
                Some(true)
            } else if self.no_fake_touch {
                Some(false)
            } else {
                None
            },
            auto_grant_permissions: if self.no_auto_grant { Some(false) } else { None },
            installer_spoof: if self.no_spoof {
                Some(None)
            } else if let Some(ref pkg) = self.installer {
                Some(Some(pkg.clone()))
            } else {
                None
            },
            abi_override: self.abi.as_ref().map(|s| Some(s.clone())),
            allow_downgrade: if self.allow_downgrade { Some(true) } else { None },
            force_sdk: if self.force_sdk { Some(true) } else { None },
        })
    }

    fn has_explicit_overrides(&self) -> bool {
        self.window_mode.is_some()
            || self.width.is_some()
            || self.height.is_some()
            || self.dpi.is_some()
            || self.fake_wifi
            || self.no_fake_wifi
            || self.fake_touch
            || self.no_fake_touch
            || self.abi.is_some()
            || self.installer.is_some()
            || self.no_spoof
            || self.no_auto_grant
            || self.allow_downgrade
            || self.force_sdk
    }
}

#[derive(Subcommand, Debug)]
enum AdaptAction {
    /// Inspect and show active/recommended adaptation profile for an APK/bundle or package
    Show { target: String },
    /// Configure and save per-app adaptation settings
    Set {
        target: String,
        #[command(flatten)]
        flags: AdaptFlags,
    },
    /// Delete saved per-app adaptation override
    Reset { package_name: String },
    /// List all saved per-app adaptation profiles
    List,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run an APK, split bundle (.xapk/.apks/.apkm), or installed Android package
    Run {
        /// Path to .apk/.xapk/.apks/.apkm file or package name (e.g. com.example.app)
        target: String,
        #[command(flatten)]
        flags: AdaptFlags,
    },
    /// Install an APK/bundle and create desktop launcher entry (.desktop)
    Install {
        /// Path to .apk/.xapk/.apks/.apkm file
        apk_path: PathBuf,
        #[command(flatten)]
        flags: AdaptFlags,
    },
    /// Inspect an APK/bundle and print metadata + compatibility adaptation report
    Info {
        /// Path to .apk/.xapk/.apks/.apkm file
        apk_path: PathBuf,
    },
    /// Manage per-app compatibility & adaptation profiles
    Adapt {
        #[command(subcommand)]
        action: AdaptAction,
    },
    /// Install Android ARM/ARM64 (libndk_translation) layer in Waydroid (requires sudo once)
    SetupArm,
    /// Initialize directories and check system requirements
    Init,
    /// Check system environment, binderfs, GPU, Wayland and session status
    Status,
    /// Stop running RustyDroid session
    Stop,
    /// Execute command or start interactive shell in Android environment
    Shell {
        /// Command arguments to run inside container
        args: Vec<String>,
    },
    /// Install and initialize graphical Waydroid runner (requires sudo once)
    SetupRunner,
    /// Internal container helper for rootless binderfs device initialization
    #[command(hide = true)]
    InternalBinderfs {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaydroidRunnerState {
    NotInstalled,
    NotInitialized,
    Stopped,
    Running,
}

fn parse_waydroid_status_output(combined: &str) -> WaydroidRunnerState {
    if combined.contains("not initialized") {
        return WaydroidRunnerState::NotInitialized;
    }

    if combined.contains("Session:\tRUNNING") || combined.contains("Session: RUNNING") {
        return WaydroidRunnerState::Running;
    }

    WaydroidRunnerState::Stopped
}

fn check_waydroid_runner_state() -> WaydroidRunnerState {
    let output = match std::process::Command::new("waydroid").arg("status").output() {
        Ok(o) => o,
        Err(_) => return WaydroidRunnerState::NotInstalled,
    };

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_waydroid_status_output(&combined)
}

fn find_runner_setup_script() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from(&home).join(".local/bin/install-waydroid-runner.sh"),
        PathBuf::from("/usr/local/bin/install-waydroid-runner.sh"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn normalize_path_str(raw: &str) -> String {
    let unescaped = if let Some(stripped) = raw.strip_prefix("file://") {
        percent_decode(stripped)
    } else {
        raw.to_string()
    };
    unescaped
}

fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            if let (Some(h1), Some(h2)) = (chars.next(), chars.next()) {
                if let Ok(val) = u8::from_str_radix(std::str::from_utf8(&[h1, h2]).unwrap_or(""), 16) {
                    bytes.push(val);
                    continue;
                }
            }
        }
        bytes.push(b);
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}

fn is_headless_or_test_env() -> bool {
    MountManager::get_wayland_socket().is_none()
}

fn notify_desktop(title: &str, body: &str) {
    if is_headless_or_test_env() {
        return;
    }
    let _ = std::process::Command::new("notify-send")
        .args(&["-i", "system-run", title, body])
        .status();
}

fn read_waydroid_session_error(log_path: &Path) -> String {
    let log_content = std::fs::read_to_string(log_path).unwrap_or_default();
    if log_content.contains("WayDroid container is not listening") {
        return "Служба контейнера Waydroid (waydroid-container) не запущена.\n👉 Запустите: sudo systemctl start waydroid-container".to_string();
    }
    let summary = log_content
        .lines()
        .last()
        .unwrap_or("Процесс сессии завершился")
        .to_string();
    format!("Сессия Waydroid завершилась с ошибкой: {}", summary)
}

fn is_systemd_user_unit_failed(unit: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-failed", "--quiet", unit])
        .stdin(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_android_boot_ready(
    mut child: Option<&mut std::process::Child>,
    via_systemd: bool,
    log_path: Option<&Path>,
    timeout_secs: u64,
) -> Result<(), String> {
    if rustydroid::runtime::adapt::is_android_user_ready() {
        return Ok(());
    }

    println!("⏳ Ожидание полной загрузки Android (sys.boot_completed=1)...");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if let Some(ref mut c) = child {
            if let Ok(Some(_)) = c.try_wait() {
                let err = log_path
                    .map(read_waydroid_session_error)
                    .unwrap_or_else(|| "Сессия Waydroid завершилась во время загрузки Android.".to_string());
                return Err(err);
            }
        }
        if via_systemd && is_systemd_user_unit_failed("rustydroid-waydroid-session") {
            return Err("Юнит systemd `rustydroid-waydroid-session` завершился с ошибкой. Проверьте `journalctl --user -u rustydroid-waydroid-session`.".to_string());
        }
        if check_waydroid_runner_state() == WaydroidRunnerState::Stopped {
            return Err("Сессия Waydroid остановилась во время загрузки Android (проверьте `waydroid status` и `journalctl`).".to_string());
        }
        AdbClient::unfreeze_waydroid_container();
        if rustydroid::runtime::adapt::is_android_user_ready() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(750));
    }

    Err("Превышено время ожидания загрузки Android внутри Waydroid (`sys.boot_completed != 1`). Проверьте `waydroid status` и `journalctl`.".to_string())
}

fn start_waydroid_session_and_wait() -> Result<(), String> {
    let log_dir = ConfigManager::get_data_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("waydroid-session.log");

    let launched_via_systemd = if let Ok(wayland_disp) = std::env::var("WAYLAND_DISPLAY") {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "reset-failed", "rustydroid-waydroid-session"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let mut srun = std::process::Command::new("systemd-run");
        srun.args([
            "--user",
            "--quiet",
            "--collect",
            "--unit=rustydroid-waydroid-session",
            &format!("--setenv=WAYLAND_DISPLAY={}", wayland_disp),
            "--",
            "waydroid",
            "session",
            "start",
        ]);
        srun.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        false
    };

    let mut child = if !launched_via_systemd {
        let mut cmd = std::process::Command::new("waydroid");
        cmd.args(&["session", "start"]);
        cmd.stdin(std::process::Stdio::null());
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        if let Ok(log_file) = std::fs::File::create(&log_path) {
            if let Ok(log_err) = log_file.try_clone() {
                cmd.stdout(std::process::Stdio::from(log_file));
                cmd.stderr(std::process::Stdio::from(log_err));
            }
        } else {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        }
        Some(
            cmd.spawn()
                .map_err(|e| format!("Не удалось запустить waydroid session start: {}", e))?,
        )
    } else {
        None
    };

    let mut session_running = false;
    for _ in 0..60 {
        if let Some(ref mut c) = child {
            if let Ok(Some(_)) = c.try_wait() {
                return Err(read_waydroid_session_error(&log_path));
            }
        }
        if launched_via_systemd && is_systemd_user_unit_failed("rustydroid-waydroid-session") {
            return Err("Юнит systemd `rustydroid-waydroid-session` не запустился. Проверьте `journalctl --user -u rustydroid-waydroid-session`.".to_string());
        }
        if check_waydroid_runner_state() == WaydroidRunnerState::Running {
            session_running = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if !session_running {
        return Err("Превышено время ожидания запуска сессии Waydroid (проверьте `waydroid status`).".to_string());
    }

    wait_for_android_boot_ready(child.as_mut(), launched_via_systemd, Some(&log_path), 90)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { apk_path } => {
            let normalized_str = normalize_path_str(&apk_path.to_string_lossy());
            let apk_path = PathBuf::from(&normalized_str);
            println!("🔍 Inspecting package: {}", apk_path.display());
            let overrides = ProfileOverrides::default();
            let (_pkg, _act, app_name, maybe_insp, profile) =
                inspect_target_or_package(&normalized_str, true, &overrides)?;
            let inspection = maybe_insp.expect("archive inspection must produce BundleInspection");
            let meta = &inspection.metadata;

            notify_desktop(
                &format!("RustyDroid: {}", app_name),
                &format!(
                    "Пакет: {}\nВерсия: {}\nABI: {}\nРазрешений: {}",
                    meta.package_name,
                    meta.version_name.as_deref().unwrap_or("1.0"),
                    if inspection.compat.native_abis.is_empty() {
                        "DEX".to_string()
                    } else {
                        inspection.compat.native_abis.join(", ")
                    },
                    meta.permissions.len()
                ),
            );

            println!("\n📋 APK Metadata:");
            println!("  • App Label:     {}", meta.app_label.as_deref().unwrap_or("N/A"));
            println!("  • Package Name:  {}", meta.package_name);
            println!("  • Version Name:  {}", meta.version_name.as_deref().unwrap_or("N/A"));
            println!("  • Version Code:  {}", meta.version_code.as_deref().unwrap_or("N/A"));
            println!("  • Main Activity: {}", meta.main_activity.as_deref().unwrap_or("N/A"));
            println!("  • Icon File:     {}", meta.icon_path_in_apk.as_deref().unwrap_or("Not found"));
            if let Some(ref icon) = meta.icon_data {
                println!("  • Icon Size:     {} bytes", icon.len());
            }

            if !meta.permissions.is_empty() {
                println!("\n🔐 Permissions ({} total):", meta.permissions.len());
                for (i, perm) in meta.permissions.iter().enumerate() {
                    let short_perm = perm.strip_prefix("android.permission.").unwrap_or(perm);
                    println!("    {}. {}", i + 1, short_perm);
                }
            }

            print_compatibility_report(&inspection, &profile);
        }

        Commands::Install { apk_path, flags } => {
            let normalized_str = normalize_path_str(&apk_path.to_string_lossy());
            let apk_path = PathBuf::from(&normalized_str);
            println!("📦 Installing package: {}", apk_path.display());
            let overrides = flags.to_overrides()?;
            let (_pkg, _act, app_name, maybe_insp, profile) =
                inspect_target_or_package(&normalized_str, true, &overrides)?;
            let inspection = maybe_insp.expect("archive inspection must produce BundleInspection");
            let meta = &inspection.metadata;

            // 1. Generate desktop file and save icon
            let desktop_file = DesktopIntegration::install_desktop_entry(meta, None)?;
            println!("✅ Desktop entry registered: {}", desktop_file.display());
            if let Some(ref icon) = meta.icon_data {
                println!("🖼️  Extracted app icon ({} bytes)", icon.len());
            }

            if flags.save_profile || flags.has_explicit_overrides() {
                let saved = ProfileStore::save(&profile)?;
                println!("⚙️  Профиль адаптации сохранён: {}", saved.display());
            }

            notify_desktop(
                "RustyDroid",
                &format!("✅ Приложение {} добавлено в меню приложений!", app_name),
            );

            // 2. Install inside running Waydroid / container if available
            if !is_headless_or_test_env() && check_waydroid_runner_state() == WaydroidRunnerState::Running {
                println!("📲 Установка адаптированного пакета в Waydroid...");
                let serial = try_connect_waydroid_adb();
                install_bundle_adapted(&inspection, &profile, serial.as_deref())?;
                println!("✅ Пакет {} установлен в Waydroid!", meta.package_name);
            } else if SessionManager::is_running() && AdbClient::is_adb_available() {
                println!("📲 Installing into running container...");
                match AdbClient::install_apk(&apk_path.to_string_lossy()) {
                    Ok(out) => println!("✅ {}", out.trim()),
                    Err(e) => eprintln!("⚠️ Container install note: {}", e),
                }
            } else {
                println!("ℹ️  Сессия не активна — пакет будет установлен автоматически при первом запуске.");
            }
        }

        Commands::Run { target, flags } => {
            let target = normalize_path_str(&target);
            let is_apk = is_package_archive_path(&target);
            if is_apk && !Path::new(&target).exists() {
                anyhow::bail!("Failed to open APK file: {}", target);
            }

            let overrides = flags.to_overrides()?;
            if is_apk {
                println!("📦 Inspecting APK: {}", target);
            } else {
                println!("📦 Launching package: {}", target);
            }
            let (package, activity, display_name, maybe_inspection, profile) =
                inspect_target_or_package(&target, is_apk, &overrides)?;

            if is_apk {
                println!("🚀 App: {} ({})", display_name, package);
                notify_desktop("RustyDroid", &format!("Запуск {}...", display_name));
                if let Some(ref insp) = maybe_inspection {
                    let _ = DesktopIntegration::install_desktop_entry(&insp.metadata, None);
                    for warn in &insp.compat.warnings {
                        println!("⚠️  {}", warn);
                    }
                }
            } else {
                notify_desktop("RustyDroid", &format!("Запуск пакета {}...", target));
            }

            if flags.save_profile {
                let saved = ProfileStore::save(&profile)?;
                println!("⚙️  Профиль адаптации сохранён: {}", saved.display());
            }

            // Check if Android system images are installed cleanly
            let img_status = ImageManager::check_status();
            if !img_status.system_img_exists {
                eprintln!("\n⚠️  ВНИМАНИЕ: Системный образ Android (system.img) не найден!");
                eprintln!("   Без системного образа (LineageOS / AOSP) Android-приложение не сможет отрисовать окно.");
                eprintln!("   👉 Установите образ командой: sudo pacman -S waydroid-image");
                eprintln!("      или поместите system.img в: {}", img_status.images_dir.display());

                notify_desktop(
                    "RustyDroid: Требуется образ Android",
                    "Установите 'waydroid-image' (sudo pacman -S waydroid-image) для запуска окна приложения",
                );
            }

            // Ensure lightweight session state bookkeeping in headless/test runs
            if is_headless_or_test_env() {
                if !SessionManager::is_running() {
                    println!("⚙️  Starting RustyDroid container session...");
                    match SessionManager::start_session() {
                        Ok(pid) => println!("🟢 Container session started (PID: {})", pid),
                        Err(e) => eprintln!("⚠️ Note starting session: {}", e),
                    }
                }
                eprintln!("ℹ️  Wayland display socket not found (graphical runner skipped in headless/sandbox mode).");
                return Ok(());
            }

            match check_waydroid_runner_state() {
                WaydroidRunnerState::NotInstalled | WaydroidRunnerState::NotInitialized => {
                    let msg = "Графический раннер Waydroid не установлен или не инициализирован. Выполните `rustydroid setup-runner`.";
                    eprintln!("\n❌ {}", msg);
                    notify_desktop("RustyDroid: Требуется настройка раннера", msg);
                    anyhow::bail!("{}", msg);
                }
                WaydroidRunnerState::Stopped => {
                    println!("🔄 Запуск сессии Waydroid на рабочем столе (первый старт Android может занять до 60 сек)...");
                    notify_desktop("RustyDroid", "Запуск сессии Android (подождите ~15–45 сек)...");
                    if let Err(err) = start_waydroid_session_and_wait() {
                        eprintln!("\n❌ {}", err);
                        notify_desktop("RustyDroid: Служба не запущена", &err);
                        anyhow::bail!("{}", err);
                    }
                }
                WaydroidRunnerState::Running => {
                    if let Err(err) = wait_for_android_boot_ready(None, false, None, 90) {
                        eprintln!("\n❌ {}", err);
                        notify_desktop("RustyDroid: Android не готов", &err);
                        anyhow::bail!("{}", err);
                    }
                }
            }

            let gralloc_diag = detect_waydroid_gralloc_diagnostic(true);
            if gralloc_diag.is_mismatch() {
                let msg = gralloc_diag.remediation_message();
                eprintln!("\n❌ {}", msg);
                notify_desktop("RustyDroid: Ошибка графики Waydroid (gralloc)", &msg);
                anyhow::bail!("{}", msg);
            }

            let serial = try_connect_waydroid_adb_with_retry(6, 1000);
            let launch_result: anyhow::Result<()> = (|| {
                if let Some(ref insp) = maybe_inspection {
                    install_bundle_adapted(insp, &profile, serial.as_deref())?;
                }
                launch_adapted_app(
                    serial.as_deref(),
                    &package,
                    activity.as_deref(),
                    &display_name,
                    &profile,
                )?;
                Ok(())
            })();

            if let Err(err) = launch_result {
                let err_text = format!("{}", err);
                notify_desktop("RustyDroid: Ошибка запуска приложения", &err_text);
                return Err(err);
            }
        }


        Commands::Init => {
            println!("🛠️  Initializing RustyDroid Environment...");
            let data_dir = ConfigManager::get_data_dir();
            MountManager::prepare_storage_tree(&data_dir)?;
            ImageManager::prepare_dirs()?;

            println!("📁 Storage tree prepared at: {}", data_dir.display());

            if BinderManager::is_supported() {
                println!("🔗 Binderfs is supported by your kernel!");
                println!("   (Binderfs will be mounted automatically inside rootless container namespace upon session start)");
            }

            let status = ImageManager::check_status();
            println!("\n📦 Android Images status:");
            println!("  • Images directory: {}", status.images_dir.display());
            println!("  • system.img:       {}", if status.system_img_exists { "Found ✅" } else { "Missing (download with `rustydroid init --download` or place in images/)" });
            println!("  • vendor.img:       {}", if status.vendor_img_exists { "Found ✅" } else { "Missing (place in images/)" });
        }

        Commands::Status => {
            println!("🔎 RustyDroid System Diagnosis:\n");

            // 1. Kernel
            let os_release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
            println!("  • Linux Kernel:       {}", os_release.trim());

            // 2. Binderfs
            let binder_ok = BinderManager::is_supported();
            println!("  • Kernel Binderfs:    {}", if binder_ok { "Supported ✅ (Rust Binder in kernel)" } else { "Not found ❌" });

            // 3. User Namespaces
            let userns_ok = NamespaceManager::is_unshare_supported();
            println!("  • User Namespaces:    {}", if userns_ok { "Supported ✅" } else { "Disabled ❌" });

            // 4. Bubblewrap
            let bwrap_ok = std::process::Command::new("bwrap").arg("--version").output().is_ok();
            println!("  • Bubblewrap (bwrap): {}", if bwrap_ok { "Available ✅" } else { "Not installed ❌" });

            // 5. Wayland
            let wl_socket = MountManager::get_wayland_socket();
            println!("  • Wayland Display:    {}", match wl_socket {
                Some(p) => format!("Active ({}) ✅", p.display()),
                None => "Not detected ⚠️".to_string(),
            });

            // 6. GPU DRI Acceleration
            let gpu_node = MountManager::verify_gpu_access();
            println!("  • GPU Acceleration:   {}", match gpu_node {
                Some(p) => format!("Available ({}) ✅", p.display()),
                None => "Missing /dev/dri/renderD128 ⚠️".to_string(),
            });

            // 7. ADB Bridge
            let adb_ok = AdbClient::is_adb_available();
            println!("  • ADB Tooling:        {}", if adb_ok { "Available ✅" } else { "Not found (optional) ⚠️" });

            // 8. Waydroid Graphical Runner & ARM Translation
            let runner_state = check_waydroid_runner_state();
            let runner_desc = match runner_state {
                WaydroidRunnerState::NotInstalled => "Not installed ❌",
                WaydroidRunnerState::NotInitialized => "Not initialized ⚠️ (run `rustydroid setup-runner`)",
                WaydroidRunnerState::Stopped => "Initialized, session STOPPED ⚪",
                WaydroidRunnerState::Running => "RUNNING 🟢",
            };
            println!("  • Graphical Runner:   {}", runner_desc);
            let gralloc_diag =
                detect_waydroid_gralloc_diagnostic(runner_state == WaydroidRunnerState::Running);
            let gralloc_label = gralloc_diag
                .runtime_gralloc
                .as_deref()
                .or(gralloc_diag.base_prop_gralloc.as_deref())
                .unwrap_or("unknown");
            if gralloc_diag.is_mismatch() {
                println!(
                    "  • Graphics Gralloc:   {} ⚠️  (mismatch с minigbm_gbm_mesa! Требуется minigbm_gbm_mesa)",
                    gralloc_label
                );
            } else {
                println!("  • Graphics Gralloc:   {} ✅", gralloc_label);
            }
            let abis = rustydroid::runtime::adapt::detect_container_abis();
            let arm_ok = rustydroid::apk::compat::container_supports_arm(&abis);
            println!(
                "  • Container ABIs:     {} ({})",
                abis.join(", "),
                if arm_ok {
                    "ARM/ARM64 ready ✅"
                } else {
                    "x86 only ⚠️ run `rustydroid setup-arm`"
                }
            );

            // 9. Session Status
            let session_info = SessionManager::get_state();
            println!("\n  • Session Status:     {}", match session_info {
                Some(s) => format!("RUNNING 🟢 (PID: {}, Mode: {})", s.pid, s.display_mode),
                None => "STOPPED ⚪".to_string(),
            });

            // 10. Images
            let img_status = ImageManager::check_status();
            println!("  • Android Images:     system.img: {}, vendor.img: {}",
                if img_status.system_img_exists { "Found ✅" } else { "None" },
                if img_status.vendor_img_exists { "Found ✅" } else { "None" }
            );
        }

        Commands::Adapt { action } => match action {
            AdaptAction::Show { target } => {
                let target = normalize_path_str(&target);
                let is_apk = is_package_archive_path(&target);
                if is_apk && !Path::new(&target).exists() {
                    anyhow::bail!("Файл пакета не найден: {}", target);
                }
                let (_pkg, _act, _label, maybe_insp, profile) =
                    inspect_target_or_package(&target, is_apk, &ProfileOverrides::default())?;
                if let Some(insp) = maybe_insp {
                    print_compatibility_report(&insp, &profile);
                } else {
                    let has_saved = ProfileStore::load(&profile.package_name).is_some();
                    println!(
                        "⚙️  Профиль адаптации для {} ({}):",
                        profile.package_name,
                        if has_saved { "сохранённый" } else { "по умолчанию" }
                    );
                    print_profile_fields(&profile);
                }
            }
            AdaptAction::Set { target, flags } => {
                let target = normalize_path_str(&target);
                let is_apk = is_package_archive_path(&target);
                let overrides = flags.to_overrides()?;
                let (pkg, _act, _label, _maybe_insp, profile) =
                    inspect_target_or_package(&target, is_apk, &overrides)?;
                let path = ProfileStore::save(&profile)?;
                println!("✅ Профиль для {} сохранён: {}", pkg, path.display());
                print_profile_fields(&profile);
            }
            AdaptAction::Reset { package_name } => {
                if ProfileStore::remove(&package_name)? {
                    println!("🗑️  Сохранённый профиль для {} удалён.", package_name);
                } else {
                    println!("ℹ️  Профиль для {} не найден.", package_name);
                }
            }
            AdaptAction::List => {
                let profiles = ProfileStore::list_all();
                if profiles.is_empty() {
                    println!("ℹ️  Сохранённых per-app профилей адаптации пока нет.");
                } else {
                    println!("📋 Профили адаптации RustyDroid ({}):", profiles.len());
                    for p in profiles {
                        println!(
                            "  • {} — mode={:?}, fake_wifi={}, fake_touch={}, installer={}",
                            p.package_name,
                            p.window_mode,
                            p.fake_wifi,
                            p.fake_touch,
                            p.installer_spoof.as_deref().unwrap_or("none")
                        );
                    }
                }
            }
        },

        Commands::SetupArm => {
            println!("🛠️  Установка слоя ARM/ARM64 трансляции (libndk_translation) для Waydroid...");
            let Some(script) = find_arm_setup_script() else {
                anyhow::bail!("Скрипт install-waydroid-arm-translation.sh не найден в ~/.local/bin/");
            };
            run_privileged_setup_script(
                &script,
                "✅ Слой ARM/ARM64 трансляции (libndk_translation) настроен!",
            )?;
        }

        Commands::Stop => {
            println!("🛑 Stopping RustyDroid session...");
            let was_running = SessionManager::is_running();
            SessionManager::stop_session()?;
            if was_running {
                println!("✅ RustyDroid session stopped.");
            } else {
                println!("ℹ️  No active session found.");
            }
        }

        Commands::Shell { args } => {
            if let Some(serial) = try_connect_waydroid_adb() {
                let cmd_slice: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let output = AdbClient::shell_for(&serial, &cmd_slice)?;
                print!("{}", output);
            } else if AdbClient::is_adb_available() && AdbClient::is_device_connected() {
                let cmd_slice: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let output = AdbClient::shell_command(&cmd_slice)?;
                print!("{}", output);
            } else {
                println!("⚠️ ADB not available. Running inside local container environment:");
                let config = ContainerConfig::default();
                let mut cmd = NamespaceManager::build_bwrap_command(&config, &["sh"])?;
                cmd.status()?;
            }
        }

        Commands::SetupRunner => {
            println!("🛠️  Настройка графического раннера Waydroid...");
            let Some(script) = find_runner_setup_script() else {
                anyhow::bail!("Скрипт установки install-waydroid-runner.sh не найден в ~/.local/bin/");
            };
            run_privileged_setup_script(
                &script,
                "✅ Раннер Waydroid настроен и запущен!",
            )?;
            let diag = detect_waydroid_gralloc_diagnostic(false);
            if diag.base_prop_gralloc.as_deref() == Some("gbm")
                && !diag.legacy_gbm_enabled
                && diag.cfg_prop_gralloc.as_deref() != Some("minigbm_gbm_mesa")
            {
                eprintln!("\n⚠️  Примечание: в `waydroid_base.prop` прописано `ro.hardware.gralloc=gbm`.");
                eprintln!("   Если Android 13 образ использует `minigbm_gbm_mesa`, добавьте `ro.hardware.gralloc = minigbm_gbm_mesa`");
                eprintln!("   в [properties] файла `/var/lib/waydroid/waydroid.cfg` и выполните `sudo waydroid upgrade -o`.");
            }
        }

        Commands::InternalBinderfs { path } => {
            if BinderManager::is_supported() {
                let _ = BinderManager::mount_binderfs(&path);
                let _ = BinderManager::setup_standard_devices(&path);
            }
        }
    }

    Ok(())
}

fn run_privileged_setup_script(script: &Path, success_msg: &str) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        let status = std::process::Command::new("sudo")
            .arg(script)
            .status()?;
        if status.success() {
            println!("{}", success_msg);
        } else {
            anyhow::bail!("Установка завершилась с ошибкой (код {:?})", status.code());
        }
    } else {
        let cmd_str = format!(
            "sudo '{}'; echo ''; read -p 'Нажмите Enter для закрытия...'",
            script.display()
        );
        let spawned = std::process::Command::new("kitty")
            .args(&["-e", "bash", "-c", &cmd_str])
            .spawn();
        if spawned.is_err() {
            let status = std::process::Command::new("pkexec").arg(script).status()?;
            if !status.success() {
                anyhow::bail!("Установка через pkexec не завершена");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustydroid::runtime::adapt::waydroid_text_has_error;

    #[test]
    fn test_normalize_file_uri_and_percent_decode() {
        assert_eq!(
            normalize_path_str("file:///home/kiri/%D0%97%D0%B0%D0%B3%D1%80%D1%83%D0%B7%D0%BA%D0%B8/My%20App.apk"),
            "/home/kiri/Загрузки/My App.apk"
        );
        assert_eq!(
            normalize_path_str("/home/kiri/test.apk"),
            "/home/kiri/test.apk"
        );
    }

    #[test]
    fn test_parse_waydroid_status_output_variants() {
        assert_eq!(
            parse_waydroid_status_output("Waydroid is not initialized, run \"waydroid init\""),
            WaydroidRunnerState::NotInitialized
        );
        assert_eq!(
            parse_waydroid_status_output("Session:\tSTOPPED\nVendor type:\tMAINLINE\n"),
            WaydroidRunnerState::Stopped
        );
        assert_eq!(
            parse_waydroid_status_output("Session:\tRUNNING\nContainer:\tRUNNING\nVendor type:\tMAINLINE\n"),
            WaydroidRunnerState::Running
        );
    }

    #[test]
    fn test_waydroid_text_has_error() {
        assert!(waydroid_text_has_error("[00:28:30] ERROR: WayDroid session is stopped"));
        assert!(waydroid_text_has_error("Failed to access IPlatform service"));
        assert!(waydroid_text_has_error("Waydroid is not initialized, run \"waydroid init\""));
        assert!(!waydroid_text_has_error(""));
        assert!(!waydroid_text_has_error("Session:\tRUNNING"));
    }
}
