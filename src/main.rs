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
    /// Internal container helper for rootless binderfs device initialization
    #[command(hide = true)]
    InternalBinderfs {
        path: PathBuf,
    },
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

fn notify_desktop(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(&["-i", "system-run", title, body])
        .status();
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
            let is_apk = target.ends_with(".apk") || Path::new(&target).exists();

            let (package, activity) = if is_apk {
                println!("📦 Inspecting APK: {}", target);
                let meta = ApkInspector::inspect(&target)?;
                let label = meta.app_label.as_deref().unwrap_or(&meta.package_name).to_string();
                println!("🚀 App: {} ({})", label, meta.package_name);

                notify_desktop("RustyDroid", &format!("Запуск {}...", label));

                // Ensure desktop integration is present
                let _ = DesktopIntegration::install_desktop_entry(&meta, None);

                (meta.package_name, meta.main_activity)
            } else {
                notify_desktop("RustyDroid", &format!("Запуск пакета {}...", target));
                (target.clone(), None)
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

            // Ensure container session is running
            if !SessionManager::is_running() {
                println!("⚙️  Starting RustyDroid container session...");
                match SessionManager::start_session() {
                    Ok(pid) => println!("🟢 Container session started (PID: {})", pid),
                    Err(e) => eprintln!("⚠️ Note starting session: {}", e),
                }
            } else {
                println!("🟢 Container session is already active.");
            }

            // If APK file, install it
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

            // Check if waydroid runner is installed on host
            let waydroid_available = std::process::Command::new("waydroid")
                .arg("status")
                .output()
                .is_ok();

            if waydroid_available {
                println!("🚀 Waydroid runner detected! Launching application window on Wayland...");
                if is_apk {
                    let _ = std::process::Command::new("waydroid").args(&["app", "install", &target]).status();
                }
                let _ = std::process::Command::new("waydroid").args(&["app", "launch", &package]).status();
                notify_desktop("RustyDroid", &format!("Окно {} открыто на рабочем столе!", package));
                return Ok(());
            }

            // Notice when waydroid executable is not installed
            eprintln!("\n❌ Образы Android найдены, но графический раннер Waydroid не установлен!");
            eprintln!("💡 Пакет `waydroid-image` содержит только файлы образов.");
            eprintln!("👉 Чтобы окно появилось на рабочем столе, установите сам раннер (всего 300 КБ):");
            eprintln!("     sudo pacman -S waydroid");
            eprintln!("     sudo waydroid init");
            eprintln!("     sudo systemctl start waydroid-container");
            eprintln!("\nПосле этого приложения будут открываться в нативных окнах Wayland!");

            notify_desktop(
                "RustyDroid: Требуется пакет waydroid",
                "Установите 300 КБ раннер: sudo pacman -S waydroid",
            );
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

            // 8. Session Status
            let session_info = SessionManager::get_state();
            println!("\n  • Session Status:     {}", match session_info {
                Some(s) => format!("RUNNING 🟢 (PID: {}, Mode: {})", s.pid, s.display_mode),
                None => "STOPPED ⚪".to_string(),
            });

            // 9. Images
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

        Commands::InternalBinderfs { path } => {
            if BinderManager::is_supported() {
                let _ = BinderManager::mount_binderfs(&path);
                let _ = BinderManager::setup_standard_devices(&path);
            }
        }
    }

    Ok(())
}
