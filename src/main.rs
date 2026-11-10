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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { apk_path } => {
            println!("🔍 Inspecting APK: {}", apk_path.display());
            let meta = ApkInspector::inspect(&apk_path)?;

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
            println!("📦 Installing APK: {}", apk_path.display());
            let meta = ApkInspector::inspect(&apk_path)?;

            // 1. Generate desktop file and save icon
            let desktop_file = DesktopIntegration::install_desktop_entry(&meta, None)?;
            println!("✅ Desktop entry registered: {}", desktop_file.display());
            if let Some(ref icon) = meta.icon_data {
                println!("🖼️  Extracted app icon ({} bytes)", icon.len());
            }

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
            let is_apk = target.ends_with(".apk") || Path::new(&target).exists();

            let (package, activity) = if is_apk {
                println!("📦 Inspecting APK: {}", target);
                let meta = ApkInspector::inspect(&target)?;
                println!("🚀 App: {} ({})", meta.app_label.as_deref().unwrap_or(&meta.package_name), meta.package_name);

                // Ensure desktop integration is present
                let _ = DesktopIntegration::install_desktop_entry(&meta, None);

                (meta.package_name, meta.main_activity)
            } else {
                (target.clone(), None)
            };

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
            if is_apk && AdbClient::is_adb_available() {
                println!("📲 Verifying installation in container...");
                let _ = AdbClient::install_apk(&target);
            }

            // Launch Activity
            if let Some(act) = activity {
                println!("▶️  Launching activity: {} ...", act);
                if AdbClient::is_adb_available() {
                    match AdbClient::start_activity(&package, &act) {
                        Ok(res) => println!("✅ {}", res.trim()),
                        Err(e) => eprintln!("⚠️ Launch note: {}", e),
                    }
                }
            } else {
                println!("▶️  Launching package: {} ...", package);
                if AdbClient::is_adb_available() {
                    let cmd = format!("monkey -p {} -c android.intent.category.LAUNCHER 1", package);
                    let _ = AdbClient::shell_command(&["sh", "-c", &cmd]);
                }
            }

            println!("✨ Done! Application window should appear on your Wayland display.");
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
