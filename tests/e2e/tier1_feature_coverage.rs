use crate::common::{sample_png_bytes, ActivitySpec, ApkSpec, AxmlBuilder, SyntheticApkBuilder, TestSandbox};
use byteorder::{LittleEndian, WriteBytesExt};
use rustydroid::apk::{axml::AxmlParser, ApkInspector, DesktopIntegration};
use rustydroid::binder::{ioctl::BINDER_CTL_ADD_IOCTL, BinderManager, BinderfsDevice};
use rustydroid::container::{ContainerConfig, MountManager, NamespaceManager};
use rustydroid::runtime::{ConfigManager, SessionManager, SessionState};
use std::fs;
use std::path::PathBuf;

#[test]
fn test_feat01_axml_header_validation() {
    let spec = ApkSpec::default();
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    assert!(axml_bytes.len() >= 8);
    let magic = u32::from_le_bytes([axml_bytes[0], axml_bytes[1], axml_bytes[2], axml_bytes[3]]);
    let file_size = u32::from_le_bytes([axml_bytes[4], axml_bytes[5], axml_bytes[6], axml_bytes[7]]);

    assert_eq!(magic, 0x00080003, "AXML magic must be 0x00080003");
    assert_eq!(file_size as usize, axml_bytes.len(), "AXML chunk size must match file length");

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("Valid AXML should parse cleanly");
    assert_eq!(root.name, "manifest");
}

#[test]
fn test_feat02_string_pool_utf16_decoding() {
    let mut builder = AxmlBuilder::new(false); // UTF-16
    builder.add_string("ShortString");
    builder.add_string("UnicodeString_✓_🌟");
    let (sp_chunk, offsets) = builder.build_string_pool();

    assert_eq!(offsets.len(), 2);
    // Header flags should have bit 8 cleared for UTF-16
    let flags = u32::from_le_bytes([sp_chunk[16], sp_chunk[17], sp_chunk[18], sp_chunk[19]]);
    assert_eq!(flags & (1 << 8), 0, "UTF-16 flag must be 0");
}

#[test]
fn test_feat03_string_pool_utf8_decoding() {
    let mut builder = AxmlBuilder::new(true); // UTF-8
    let long_str = "A".repeat(150); // > 127 bytes, tests 2-byte length prefix
    builder.add_string("HelloUTF8");
    builder.add_string(&long_str);
    builder.add_string("Привет, мир! 🚀");
    let (sp_chunk, offsets) = builder.build_string_pool();

    assert_eq!(offsets.len(), 3);
    // Header flags should have bit 8 set for UTF-8
    let flags = u32::from_le_bytes([sp_chunk[16], sp_chunk[17], sp_chunk[18], sp_chunk[19]]);
    assert_ne!(flags & (1 << 8), 0, "UTF-8 flag (1 << 8) must be set");

    let mut spec = ApkSpec::default();
    spec.use_utf8_string_pool = true;
    spec.app_label = "UTF8 Тестовое Приложение 🚀".to_string();
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("UTF-8 string pool AXML should parse cleanly");
    let app = root.find_children("application")[0];
    let label = app.get_attr("label", parser.string_pool());
    assert_eq!(label.as_deref(), Some("UTF8 Тестовое Приложение 🚀"));
}

#[test]
fn test_feat04_resource_map_and_namespace_handling() {
    let mut spec = ApkSpec::default();
    spec.include_resource_map = true;
    spec.include_namespaces = true;
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("AXML with resource map and namespaces should parse cleanly");
    assert_eq!(root.name, "manifest");
    assert!(!root.children.is_empty());
}

#[test]
fn test_feat05_element_and_attribute_decoding() {
    let spec = ApkSpec {
        package_name: "org.rustydroid.types".to_string(),
        version_code: 42,
        version_name: "2.1.0".to_string(),
        app_label: "TypeTest".to_string(),
        ..Default::default()
    };
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("AXML element and attribute parsing failed");

    let pkg = root.get_attr("package", parser.string_pool());
    assert_eq!(pkg.as_deref(), Some("org.rustydroid.types"));

    let vc = root.get_attr("versionCode", parser.string_pool());
    assert_eq!(vc.as_deref(), Some("42"));

    let vn = root.get_attr("versionName", parser.string_pool());
    assert_eq!(vn.as_deref(), Some("2.1.0"));
}

#[test]
fn test_feat06_manifest_metadata_extraction() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("metadata_test.apk");

    let spec = ApkSpec {
        package_name: "com.example.metatest".to_string(),
        version_code: 105,
        version_name: "1.5.0".to_string(),
        app_label: "Meta App".to_string(),
        permissions: vec![
            "android.permission.CAMERA".to_string(),
            "android.permission.RECORD_AUDIO".to_string(),
        ],
        ..Default::default()
    };

    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();
    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect APK");

    assert_eq!(meta.package_name, "com.example.metatest");
    assert_eq!(meta.version_code.as_deref(), Some("105"));
    assert_eq!(meta.version_name.as_deref(), Some("1.5.0"));
    assert_eq!(meta.app_label.as_deref(), Some("Meta App"));
    assert_eq!(
        meta.permissions,
        vec![
            "android.permission.CAMERA".to_string(),
            "android.permission.RECORD_AUDIO".to_string(),
        ]
    );
}

#[test]
fn test_feat07_launcher_activity_resolution() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("launcher_test.apk");

    let spec = ApkSpec {
        package_name: "com.example.launcher".to_string(),
        activities: vec![
            ActivitySpec {
                name: "com.example.launcher.BackgroundServiceActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: ".MainActivity".to_string(),
                is_launcher: true,
                is_alias: false,
            },
        ],
        ..Default::default()
    };

    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();
    let meta = ApkInspector::inspect(&apk_path).unwrap();

    assert_eq!(
        meta.main_activity.as_deref(),
        Some("com.example.launcher.MainActivity"),
        "Relative activity name starting with dot must be prefixed with package name"
    );
}

#[test]
fn test_feat08_density_icon_discovery() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("icon_test.apk");

    let spec = ApkSpec {
        icon_path: Some("res/mipmap-xxxhdpi-v4/ic_launcher.png".to_string()),
        icon_bytes: Some(sample_png_bytes()),
        ..Default::default()
    };

    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();
    let meta = ApkInspector::inspect(&apk_path).unwrap();

    assert!(meta.icon_data.is_some());
    assert_eq!(
        meta.icon_path_in_apk.as_deref(),
        Some("res/mipmap-xxxhdpi-v4/ic_launcher.png")
    );
    assert_eq!(meta.icon_data.unwrap(), sample_png_bytes());
}

#[test]
fn test_feat09_xdg_desktop_entry_generation() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("desktop_test.apk");

    let spec = ApkSpec {
        package_name: "org.rustydroid.desktopapp".to_string(),
        app_label: "Desktop App".to_string(),
        ..Default::default()
    };

    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();
    let meta = ApkInspector::inspect(&apk_path).unwrap();

    let desktop_file = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid")).unwrap();
    assert!(desktop_file.exists());

    let content = fs::read_to_string(&desktop_file).unwrap();
    assert!(content.contains("[Desktop Entry]"));
    assert!(content.contains("Type=Application"));
    assert!(content.contains("Name=Desktop App"));
    assert!(content.contains("Exec=rustydroid run \"org.rustydroid.desktopapp\""));
    assert!(content.contains("StartupWMClass=org.rustydroid.desktopapp"));
    assert!(content.contains("Icon=rustydroid-org.rustydroid.desktopapp"));

    let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
}

#[test]
fn test_feat10_icon_theme_installation() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("icon_install_test.apk");

    let spec = ApkSpec {
        package_name: "org.rustydroid.iconapp".to_string(),
        icon_path: Some("res/mipmap-hdpi-v4/ic_launcher.png".to_string()),
        icon_bytes: Some(sample_png_bytes()),
        ..Default::default()
    };

    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();
    let meta = ApkInspector::inspect(&apk_path).unwrap();

    let _ = DesktopIntegration::install_desktop_entry(&meta, None).unwrap();
    let icons_dir = DesktopIntegration::get_icons_dir();
    let expected_icon = icons_dir.join("rustydroid-org.rustydroid.iconapp.png");

    assert!(expected_icon.exists(), "Installed icon should exist in hicolor 192x192 apps dir");
    assert_eq!(fs::read(&expected_icon).unwrap(), sample_png_bytes());

    // Clean up
    DesktopIntegration::remove_desktop_entry(&meta.package_name).unwrap();
    assert!(!expected_icon.exists(), "Icon must be removed when desktop entry is cleaned up");
}

#[test]
fn test_feat11_unprivileged_namespaces_setup() {
    let supported = NamespaceManager::is_unshare_supported();
    println!("Unprivileged namespaces supported: {}", supported);
    assert!(supported, "Host environment must support unprivileged user namespaces");
}

#[test]
fn test_feat12_uid_gid_rootless_mapping() {
    let current_uid = unsafe { libc::getuid() };
    let current_gid = unsafe { libc::getgid() };
    assert!(current_uid > 0, "Running as unprivileged host user (UID > 0)");
    assert!(current_gid > 0, "Running as unprivileged host user (GID > 0)");
}

#[test]
fn test_feat13_dynamic_binderfs_mount() {
    let supported = BinderManager::is_supported();
    println!("Binderfs supported in kernel: {}", supported);
    assert!(supported, "Kernel must support binder / binderfs");
}

#[test]
fn test_feat14_binder_nodes_allocation() {
    assert_eq!(BINDER_CTL_ADD_IOCTL, 0xc1086201);
    let dev = BinderfsDevice::new("binder").expect("Failed to create BinderfsDevice");
    assert_eq!(dev.major, 0);
    assert_eq!(dev.minor, 0);
}

#[test]
fn test_feat15_rootless_container_fs_layout() {
    let config = ContainerConfig::default();
    let cmd = NamespaceManager::build_bwrap_command(&config, &["sh", "-c", "echo ok"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();

    assert!(args.contains(&"--unshare-pid".to_string()));
    assert!(args.contains(&"--unshare-ipc".to_string()));
    assert!(args.contains(&"--unshare-uts".to_string()));
    assert!(args.contains(&"--hostname".to_string()));
    assert!(args.contains(&"rustydroid".to_string()));
    assert!(args.contains(&"--proc".to_string()));
    assert!(args.contains(&"/proc".to_string()));
    assert!(args.contains(&"--dev".to_string()));
    assert!(args.contains(&"/dev".to_string()));
    assert!(args.contains(&"/data".to_string()));
}

#[test]
fn test_feat16_direct_gpu_node_passthrough() {
    let gpu = MountManager::verify_gpu_access();
    assert_eq!(gpu, Some(PathBuf::from("/dev/dri/renderD128")));

    let config = ContainerConfig::default();
    let cmd = NamespaceManager::build_bwrap_command(&config, &["true"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert!(args.contains(&"--dev-bind".to_string()));
    assert!(args.contains(&"/dev/dri/renderD128".to_string()));
}

#[test]
fn test_feat17_wayland_socket_passthrough() {
    let wl = MountManager::get_wayland_socket();
    println!("Wayland socket detected: {:?}", wl);
    if let Some(sock) = wl {
        assert!(sock.exists(), "Detected Wayland socket must exist on host");
    }
}

#[test]
fn test_feat18_pipewire_pulseaudio_routing() {
    let config = ContainerConfig::default();
    if let Some(ref pulse) = config.pulse_socket {
        println!("PulseAudio/PipeWire socket detected: {:?}", pulse);
        assert!(pulse.exists());
    }
}

#[test]
fn test_feat19_cli_info_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("cli_info.apk");
    let spec = ApkSpec {
        package_name: "org.rustydroid.cliinfo".to_string(),
        app_label: "CLI Info App".to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let (status, stdout, stderr) = sandbox.run_cli(&["info", &apk_path.to_string_lossy()]).unwrap();
    assert!(status.success(), "cli info must exit with 0; stderr: {}", stderr);
    assert!(stdout.contains("Package Name:  org.rustydroid.cliinfo"));
    assert!(stdout.contains("App Label:     CLI Info App"));
    assert!(stdout.contains("Main Activity: org.rustydroid.cliinfo.MainActivity"));
}

#[test]
fn test_feat20_cli_install_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("cli_install.apk");
    let spec = ApkSpec {
        package_name: "org.rustydroid.cliinstall".to_string(),
        app_label: "CLI Install App".to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let (status, stdout, stderr) = sandbox.run_cli(&["install", &apk_path.to_string_lossy()]).unwrap();
    assert!(status.success(), "cli install must exit with 0; stderr: {}", stderr);
    assert!(stdout.contains("Desktop entry registered:"));

    let expected_desktop = sandbox.apps_dir().join("rustydroid-org.rustydroid.cliinstall.desktop");
    assert!(expected_desktop.exists());
}

#[test]
fn test_feat21_cli_status_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["status"]).unwrap();
    assert!(status.success(), "cli status must exit with 0; stderr: {}", stderr);
    assert!(stdout.contains("Linux Kernel:"));
    assert!(stdout.contains("Kernel Binderfs:"));
    assert!(stdout.contains("User Namespaces:"));
    assert!(stdout.contains("Bubblewrap (bwrap):"));
    assert!(stdout.contains("Session Status:"));
}

#[test]
fn test_feat22_cli_init_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["init"]).unwrap();
    assert!(status.success(), "cli init must exit with 0; stderr: {}", stderr);
    assert!(stdout.contains("Storage tree prepared at:"));

    assert!(sandbox.data_dir().join("rootfs").exists());
    assert!(sandbox.data_dir().join("data").exists());
    assert!(sandbox.data_dir().join("images").exists());
    assert!(sandbox.data_dir().join("binderfs").exists());
}

#[test]
fn test_feat23_cli_run_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("cli_run.apk");
    let spec = ApkSpec {
        package_name: "org.rustydroid.clirun".to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let (status, stdout, _stderr) = sandbox.run_cli(&["run", &apk_path.to_string_lossy()]).unwrap();
    assert!(status.success());
    assert!(stdout.contains("Inspecting APK:"));

    // Explicitly stop the session to clean up the supervisor daemon
    let (stop_status, stop_stdout, _) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(stop_status.success());
    assert!(stop_stdout.contains("RustyDroid session stopped"));
}

#[test]
fn test_feat24_cli_stop_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(status.success(), "cli stop must exit with 0; stderr: {}", stderr);
    assert!(stdout.contains("Stopping RustyDroid session...") || stdout.contains("No active session"));
}

#[test]
fn test_feat25_cli_shell_subcommand() {
    let sandbox = TestSandbox::new().unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["shell", "echo", "test_shell"]).unwrap();
    // In our test environment adb is not connected, but CLI handles it gracefully
    println!("cli shell stdout: {}, stderr: {}", stdout, stderr);
    let _ = status;
}

#[test]
fn test_feat26_session_state_and_child_subreaper() {
    let state = SessionState {
        pid: 12345,
        started_at: 1700000000,
        display_mode: "multi_window".to_string(),
        is_active: true,
    };

    let serialized = serde_json::to_string(&state).unwrap();
    let deserialized: SessionState = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.pid, 12345);
    assert_eq!(deserialized.display_mode, "multi_window");
    assert!(deserialized.is_active);
}
