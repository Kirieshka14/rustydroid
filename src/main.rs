mod adb;
mod apk;
mod binder;
mod container;
mod runtime;

use adb::AdbClient;
use apk::{ApkInspector, DesktopIntegration};
use binder::BinderManager;
use clap::{Parser, Subcommand};
use container::{ContainerConfig, MountManager, NamespaceManager};
use runtime::{ConfigManager, ImageManager, SessionManager};
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

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run an APK file or installed Android application
    Run {
        /// Path to .apk file or package name (e.g. com.example.app)
        target: String,
    },
    /// Install an APK and create desktop launcher entry (.desktop)
    Install {
        /// Path to .apk file
        apk_path: PathBuf,
    },
    /// Inspect an APK file and print metadata (package, Activity, permissions, icon)
    Info {
        /// Path to .apk file
        apk_path: PathBuf,
    },
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

fn waydroid_text_has_error(combined: &str) -> bool {
    combined.contains("ERROR:")
        || combined.contains("WayDroid session is stopped")
        || combined.contains("Failed to access IPlatform")
        || combined.contains("not initialized")
}

fn waydroid_output_has_error(out: &std::process::Output) -> bool {
    if !out.status.success() {
        return true;
    }
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    waydroid_text_has_error(&combined)
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

fn launch_in_waydroid(target: &str, package: &str, display_name: &str, is_apk: bool) -> anyhow::Result<()> {
    if is_apk {
        println!("📲 Установка APK в Waydroid: {} ...", target);
        let install_out = std::process::Command::new("waydroid")
            .args(&["app", "install", target])
            .output()?;
        if waydroid_output_has_error(&install_out) {
            let msg = format!(
                "{}{}",
                String::from_utf8_lossy(&install_out.stdout),
                String::from_utf8_lossy(&install_out.stderr)
            );
            eprintln!("❌ Ошибка установки APK в Waydroid:\n{}", msg.trim());
            notify_desktop(
                "RustyDroid: Ошибка установки APK",
                &format!("Не удалось установить {}", display_name),
            );
            return Ok(());
        }
    }

    println!("🚀 Запуск окна приложения: {} ({}) ...", display_name, package);
    let launch_out = std::process::Command::new("waydroid")
        .args(&["app", "launch", package])
        .output()?;

    if waydroid_output_has_error(&launch_out) {
        let msg = format!(
            "{}{}",
            String::from_utf8_lossy(&launch_out.stdout),
            String::from_utf8_lossy(&launch_out.stderr)
        );
        eprintln!("❌ Ошибка при запуске приложения в Waydroid:\n{}", msg.trim());
        notify_desktop(
            "RustyDroid: Ошибка запуска",
            &format!("Не удалось открыть {}", display_name),
        );
    } else {
        println!("✅ Приложение успешно запущено!");
        notify_desktop("RustyDroid", &format!("Окно {} открыто на рабочем столе!", display_name));
    }
    Ok(())
}

fn start_waydroid_session_and_wait() -> Result<(), String> {
    let log_dir = ConfigManager::get_data_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("waydroid-session.log");

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

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Не удалось запустить waydroid session start: {}", e))?;

    for _ in 0..120 {
        if let Ok(Some(_)) = child.try_wait() {
            let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log_content.contains("WayDroid container is not listening") {
                return Err("Служба контейнера Waydroid (waydroid-container) не запущена.\n👉 Запустите: sudo systemctl start waydroid-container".to_string());
            }
            let summary = log_content.lines().last().unwrap_or("Процесс сессии завершился").to_string();
            return Err(format!("Сессия Waydroid завершилась с ошибкой: {}", summary));
        }
        if check_waydroid_runner_state() == WaydroidRunnerState::Running {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    Err("Превышено время ожидания запуска сессии Waydroid (проверьте `waydroid status`).".to_string())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { apk_path } => {
            let normalized_str = normalize_path_str(&apk_path.to_string_lossy());
            let apk_path = PathBuf::from(&normalized_str);
            println!("🔍 Inspecting APK: {}", apk_path.display());
            let meta = ApkInspector::inspect(&apk_path)?;
            let app_name = meta.app_label.as_deref().unwrap_or(&meta.package_name);

            notify_desktop(
                &format!("RustyDroid: {}", app_name),
                &format!(
                    "Пакет: {}\nВерсия: {}\nРазрешений: {}",
                    meta.package_name,
                    meta.version_name.as_deref().unwrap_or("1.0"),
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
        }

        Commands::Install { apk_path } => {
            let normalized_str = normalize_path_str(&apk_path.to_string_lossy());
            let apk_path = PathBuf::from(&normalized_str);
            println!("📦 Installing APK: {}", apk_path.display());
            let meta = ApkInspector::inspect(&apk_path)?;
            let app_name = meta.app_label.as_deref().unwrap_or(&meta.package_name);

            // 1. Generate desktop file and save icon
            let desktop_file = DesktopIntegration::install_desktop_entry(&meta, None)?;
            println!("✅ Desktop entry registered: {}", desktop_file.display());
            if let Some(ref icon) = meta.icon_data {
                println!("🖼️  Extracted app icon ({} bytes)", icon.len());
            }

            notify_desktop(
                "RustyDroid",
                &format!("✅ Приложение {} добавлено в меню приложений!", app_name),
            );

            // 2. Install inside running container if session is active
            if SessionManager::is_running() && AdbClient::is_adb_available() {
                println!("📲 Installing into running container...");
                match AdbClient::install_apk(&apk_path.to_string_lossy()) {
                    Ok(out) => println!("✅ {}", out.trim()),
                    Err(e) => eprintln!("⚠️ Container install note: {}", e),
                }
            } else {
                println!("ℹ️  Session not active. The app will be installed on first run.");
            }
        }

        Commands::Run { target } => {
            let target = normalize_path_str(&target);
            let is_apk = target.to_lowercase().ends_with(".apk") || Path::new(&target).is_file();

            let (package, activity, display_name) = if is_apk {
                println!("📦 Inspecting APK: {}", target);
                let meta = ApkInspector::inspect(&target)?;
                let label = meta.app_label.as_deref().unwrap_or(&meta.package_name).to_string();
                println!("🚀 App: {} ({})", label, meta.package_name);

                notify_desktop("RustyDroid", &format!("Запуск {}...", label));

                // Ensure desktop integration is present
                let _ = DesktopIntegration::install_desktop_entry(&meta, None);

                (meta.package_name, meta.main_activity, label)
            } else {
                notify_desktop("RustyDroid", &format!("Запуск пакета {}...", target));
                (target.clone(), None, target.clone())
            };

            // Check if Android system images are installed
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

            // Ensure container session supervisor is running
            if !SessionManager::is_running() {
                println!("⚙️  Starting RustyDroid container session...");
                match SessionManager::start_session() {
                    Ok(pid) => println!("🟢 Container session started (PID: {})", pid),
                    Err(e) => eprintln!("⚠️ Note starting session: {}", e),
                }
            } else {
                println!("🟢 Container session is already active.");
            }

            // If APK file, install it via ADB if available
            if is_apk && AdbClient::is_adb_available() && AdbClient::is_device_connected() {
                println!("📲 Verifying installation in container...");
                let _ = AdbClient::install_apk(&target);
            }

            // Launch Activity or Package
            if let Some(ref act) = activity {
                println!("▶️  Launching activity: {} ...", act);
                if AdbClient::is_adb_available() && AdbClient::is_device_connected() {
                    let _ = AdbClient::start_activity(&package, act);
                }
            } else {
                println!("▶️  Launching package: {} ...", package);
                if AdbClient::is_adb_available() && AdbClient::is_device_connected() {
                    let cmd = format!("monkey -p {} -c android.intent.category.LAUNCHER 1", package);
                    let _ = AdbClient::shell_command(&["sh", "-c", &cmd]);
                }
            }

            if is_headless_or_test_env() {
                eprintln!("ℹ️  Wayland display socket not found (graphical runner skipped in headless/sandbox mode).");
                return Ok(());
            }

            match check_waydroid_runner_state() {
                WaydroidRunnerState::NotInstalled | WaydroidRunnerState::NotInitialized => {
                    eprintln!("\n❌ Графический раннер Waydroid не установлен или не инициализирован!");
                    eprintln!("💡 Пакеты раннера и образы Android уже подготовлены локально.");
                    eprintln!("👉 Для одноразовой настройки (установка из кэша + init + запуск службы) выполните:");
                    eprintln!("     rustydroid setup-runner");

                    notify_desktop(
                        "RustyDroid: Требуется настройка раннера",
                        "Запустите `rustydroid setup-runner` для инициализации Waydroid",
                    );
                }
                WaydroidRunnerState::Stopped => {
                    println!("🔄 Запуск сессии Waydroid на рабочем столе (первый старт Android может занять до 30 сек)...");
                    notify_desktop("RustyDroid", "Запуск сессии Android (подождите ~15–30 сек)...");
                    match start_waydroid_session_and_wait() {
                        Ok(()) => {
                            launch_in_waydroid(&target, &package, &display_name, is_apk)?;
                        }
                        Err(err) => {
                            eprintln!("\n❌ {}", err);
                            notify_desktop("RustyDroid: Служба не запущена", &err);
                        }
                    }
                }
                WaydroidRunnerState::Running => {
                    launch_in_waydroid(&target, &package, &display_name, is_apk)?;
                }
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

            // 8. Waydroid Graphical Runner
            let runner_desc = match check_waydroid_runner_state() {
                WaydroidRunnerState::NotInstalled => "Not installed ❌",
                WaydroidRunnerState::NotInitialized => "Not initialized ⚠️ (run `rustydroid setup-runner`)",
                WaydroidRunnerState::Stopped => "Initialized, session STOPPED ⚪",
                WaydroidRunnerState::Running => "RUNNING 🟢",
            };
            println!("  • Graphical Runner:   {}", runner_desc);

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
            if AdbClient::is_adb_available() {
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

            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                let status = std::process::Command::new("sudo")
                    .arg(&script)
                    .status()?;
                if status.success() {
                    println!("✅ Раннер Waydroid настроен и запущен!");
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
                    let status = std::process::Command::new("pkexec").arg(&script).status()?;
                    if !status.success() {
                        anyhow::bail!("Установка через pkexec не завершена");
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
