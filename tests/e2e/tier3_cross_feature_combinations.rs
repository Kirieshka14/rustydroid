use crate::common::{sample_png_bytes, ActivitySpec, ApkSpec, SyntheticApkBuilder, TestSandbox};
use rustydroid::apk::{ApkInspector, DesktopIntegration};
use rustydroid::container::{ContainerConfig, NamespaceManager};
use rustydroid::runtime::{AppConfig, ConfigManager, DisplayMode, SessionManager, SessionState};
use std::fs;
use std::path::PathBuf;

#[test]
fn test_tier3_apk_parse_to_desktop_and_icon() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("full_cycle.apk");

    let spec = ApkSpec {
        package_name: "org.rustydroid.fullcycle".to_string(),
        version_code: 202,
        version_name: "2.0.2".to_string(),
        app_label: "Full Cycle App".to_string(),
        permissions: vec!["android.permission.INTERNET".to_string()],
        icon_path: Some("res/mipmap-xxxhdpi-v4/ic_launcher.png".to_string()),
        icon_bytes: Some(sample_png_bytes()),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    // 1. Inspect APK
    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect APK");
    assert_eq!(meta.package_name, "org.rustydroid.fullcycle");
    assert!(meta.icon_data.is_some());

    // 2. Install desktop entry
    let desktop_file = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid")).unwrap();
    assert!(desktop_file.exists());

    let icon_file = DesktopIntegration::get_icons_dir().join("rustydroid-org.rustydroid.fullcycle.png");
    assert!(icon_file.exists());
    assert_eq!(fs::read(&icon_file).unwrap(), sample_png_bytes());

    // 3. Remove desktop entry
    DesktopIntegration::remove_desktop_entry(&meta.package_name).unwrap();
    assert!(!desktop_file.exists(), "Desktop entry must be deleted");
    assert!(!icon_file.exists(), "Icon file must be deleted");
}

#[test]
fn test_tier3_apk_with_multiple_activities_and_permissions() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("complex_app.apk");

    let spec = ApkSpec {
        package_name: "com.complex.suite".to_string(),
        app_label: "Complex Suite".to_string(),
        permissions: vec![
            "android.permission.INTERNET".to_string(),
            "android.permission.CAMERA".to_string(),
            "android.permission.ACCESS_FINE_LOCATION".to_string(),
            "android.permission.RECORD_AUDIO".to_string(),
            "android.permission.BLUETOOTH".to_string(),
            "android.permission.READ_EXTERNAL_STORAGE".to_string(),
            "android.permission.WRITE_EXTERNAL_STORAGE".to_string(),
            "android.permission.VIBRATE".to_string(),
            "android.permission.WAKE_LOCK".to_string(),
            "android.permission.POST_NOTIFICATIONS".to_string(),
        ],
        activities: vec![
            ActivitySpec {
                name: "com.complex.suite.SplashActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: "com.complex.suite.SettingsActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: ".DashboardActivity".to_string(),
                is_launcher: true,
                is_alias: false,
            },
            ActivitySpec {
                name: "com.complex.suite.AuthActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
        ],
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let meta = ApkInspector::inspect(&apk_path).unwrap();
    assert_eq!(meta.permissions.len(), 10, "All 10 permissions must be captured");
    assert_eq!(
        meta.main_activity.as_deref(),
        Some("com.complex.suite.DashboardActivity"),
        "Only the activity with MAIN+LAUNCHER must be selected"
    );
}

#[test]
fn test_tier3_cli_init_then_status() {
    let sandbox = TestSandbox::new().unwrap();

    // 1. Run init
    let (init_status, init_out, _) = sandbox.run_cli(&["init"]).unwrap();
    assert!(init_status.success());
    assert!(init_out.contains("Storage tree prepared at:"));

    // 2. Run status
    let (status_status, status_out, _) = sandbox.run_cli(&["status"]).unwrap();
    assert!(status_status.success());
    assert!(status_out.contains("Linux Kernel:"));
    assert!(status_out.contains("Kernel Binderfs:"));
    assert!(status_out.contains("Session Status:"));
}

#[test]
fn test_tier3_cli_install_then_run_package() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("pair_test.apk");
    let spec = ApkSpec {
        package_name: "org.rustydroid.pairtest".to_string(),
        app_label: "Pair Test App".to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    // 1. Install
    let (inst_status, inst_out, _) = sandbox.run_cli(&["install", &apk_path.to_string_lossy()]).unwrap();
    assert!(inst_status.success());
    assert!(inst_out.contains("Desktop entry registered:"));

    // 2. Run package name
    let (run_status, run_out, _) = sandbox.run_cli(&["run", "org.rustydroid.pairtest"]).unwrap();
    assert!(run_status.success());
    assert!(run_out.contains("Launching package: org.rustydroid.pairtest"));

    // Explicitly stop the session to clean up the supervisor daemon
    let (stop_status, stop_stdout, _) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(stop_status.success());
    assert!(stop_stdout.contains("RustyDroid session stopped"));
}

#[test]
fn test_tier3_container_config_with_wayland_gpu_audio() {
    let config = ContainerConfig {
        rootfs_path: PathBuf::from("/tmp/test_rootfs"),
        binderfs_path: PathBuf::from("/tmp/test_binderfs"),
        wayland_socket: Some("wayland-1".to_string()),
        dri_render_node: Some(PathBuf::from("/dev/dri/renderD128")),
        pulse_socket: Some(PathBuf::from("/run/user/1000/pulse/native")),
        data_dir: PathBuf::from("/tmp/test_data"),
        hostname: "rustydroid-box".to_string(),
    };

    let cmd = NamespaceManager::build_bwrap_command(&config, &["echo", "ready"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();

    assert!(args.contains(&"--hostname".to_string()));
    assert!(args.contains(&"rustydroid-box".to_string()));
    assert!(args.contains(&"--unshare-pid".to_string()));
    assert!(args.contains(&"--dev".to_string()));
    assert!(args.contains(&"/dev".to_string()));
}

#[test]
fn test_tier3_session_state_lifecycle() {
    let sandbox = TestSandbox::new().unwrap();
    let session_file = sandbox.data_dir().join("session.json");

    let state = SessionState {
        pid: std::process::id(), // current test process PID is alive!
        started_at: 1700000000,
        display_mode: "multi_window".to_string(),
        is_active: true,
    };

    let json = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&session_file, json).unwrap();
    assert!(session_file.exists());

    let loaded: SessionState = serde_json::from_str(&fs::read_to_string(&session_file).unwrap()).unwrap();
    assert_eq!(loaded.pid, std::process::id());
    assert_eq!(loaded.display_mode, "multi_window");

    // Clean up
    let _ = fs::remove_file(&session_file);
    assert!(!session_file.exists());
}

#[test]
fn test_tier3_config_roundtrip_persistence() {
    let custom_cfg = AppConfig {
        display_mode: DisplayMode::SingleWindow,
        enable_gpu: true,
        enable_audio: false,
        system_image_url: "https://example.com/system.img".to_string(),
        vendor_image_url: "https://example.com/vendor.img".to_string(),
    };

    let json = serde_json::to_string_pretty(&custom_cfg).unwrap();
    let parsed: AppConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.display_mode, DisplayMode::SingleWindow);
    assert_eq!(parsed.enable_gpu, true);
    assert_eq!(parsed.enable_audio, false);
    assert_eq!(parsed.system_image_url, "https://example.com/system.img");
}
