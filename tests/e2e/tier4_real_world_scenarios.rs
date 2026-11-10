use crate::common::{sample_png_bytes, ActivitySpec, ApkSpec, SyntheticApkBuilder, TestSandbox};
use rustydroid::apk::{ApkInspector, DesktopIntegration};
use std::collections::HashMap;
use std::fs;

#[test]
fn test_tier4_fdroid_like_workload() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("fdroid_sim.apk");

    let fdroid_permissions = vec![
        "android.permission.INTERNET".to_string(),
        "android.permission.ACCESS_NETWORK_STATE".to_string(),
        "android.permission.ACCESS_WIFI_STATE".to_string(),
        "android.permission.CHANGE_WIFI_MULTICAST_STATE".to_string(),
        "android.permission.BLUETOOTH".to_string(),
        "android.permission.BLUETOOTH_ADMIN".to_string(),
        "android.permission.CAMERA".to_string(),
        "android.permission.WAKE_LOCK".to_string(),
        "android.permission.FOREGROUND_SERVICE".to_string(),
        "android.permission.REQUEST_INSTALL_PACKAGES".to_string(),
        "android.permission.REQUEST_DELETE_PACKAGES".to_string(),
        "android.permission.RECEIVE_BOOT_COMPLETED".to_string(),
        "android.permission.POST_NOTIFICATIONS".to_string(),
    ];

    let mut extra_files = HashMap::new();
    // In real F-Droid 2.0.2, the icon is in assets/swap-icon.png
    extra_files.insert("assets/swap-icon.png".to_string(), sample_png_bytes());

    let spec = ApkSpec {
        package_name: "org.fdroid.fdroid".to_string(),
        version_code: 2000052,
        version_name: "2.0.2".to_string(),
        app_label: "F-Droid".to_string(),
        permissions: fdroid_permissions,
        activities: vec![
            ActivitySpec {
                name: "org.fdroid.fdroid.views.main.MainActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: "org.fdroid.IconCalculatorActivity".to_string(),
                is_launcher: true,
                is_alias: true,
            },
        ],
        icon_path: None, // relies on fallback search in archive
        icon_bytes: None,
        extra_files,
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    // 1. In-process inspection
    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect F-Droid simulated APK");
    assert_eq!(meta.package_name, "org.fdroid.fdroid");
    assert_eq!(meta.version_code.as_deref(), Some("2000052"));
    assert_eq!(meta.version_name.as_deref(), Some("2.0.2"));
    assert_eq!(meta.app_label.as_deref(), Some("F-Droid"));
    assert_eq!(meta.main_activity.as_deref(), Some("org.fdroid.IconCalculatorActivity"));
    assert_eq!(meta.permissions.len(), 13);
    assert_eq!(meta.icon_path_in_apk.as_deref(), Some("assets/swap-icon.png"));
    assert!(meta.icon_data.is_some());

    // 2. CLI inspection
    let (info_status, info_out, _) = sandbox.run_cli(&["info", &apk_path.to_string_lossy()]).unwrap();
    assert!(info_status.success());
    assert!(info_out.contains("Package Name:  org.fdroid.fdroid"));
    assert!(info_out.contains("Version Code:  2000052"));
    assert!(info_out.contains("Main Activity: org.fdroid.IconCalculatorActivity"));
    assert!(info_out.contains("Icon File:     assets/swap-icon.png"));
}

#[test]
fn test_tier4_aurora_store_like_workload() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("aurora_store_sim.apk");

    let spec = ApkSpec {
        package_name: "com.aurora.store".to_string(),
        version_code: 49,
        version_name: "4.4.2".to_string(),
        app_label: "Aurora Store 🛒".to_string(),
        permissions: vec![
            "android.permission.INTERNET".to_string(),
            "android.permission.REQUEST_INSTALL_PACKAGES".to_string(),
            "android.permission.UPDATE_PACKAGES_WITHOUT_USER_ACTION".to_string(),
        ],
        activities: vec![ActivitySpec {
            name: "com.aurora.store.view.ui.all.MainActivity".to_string(),
            is_launcher: true,
            is_alias: false,
        }],
        use_utf8_string_pool: true,
        icon_path: Some("res/mipmap-xxxhdpi-v4/ic_launcher.png".to_string()),
        icon_bytes: Some(sample_png_bytes()),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    // 1. Install via CLI
    let (inst_status, inst_out, _) = sandbox.run_cli(&["install", &apk_path.to_string_lossy()]).unwrap();
    assert!(inst_status.success(), "CLI install must succeed; output: {}", inst_out);

    let desktop_file = sandbox.apps_dir().join("rustydroid-com.aurora.store.desktop");
    assert!(desktop_file.exists());

    let content = fs::read_to_string(&desktop_file).unwrap();
    assert!(content.contains("Name=Aurora Store 🛒"));
    assert!(content.contains("Exec=rustydroid run \"com.aurora.store\""));
    assert!(content.contains("StartupWMClass=com.aurora.store"));

    let icon_file = sandbox.icons_dir().join("rustydroid-com.aurora.store.png");
    assert!(icon_file.exists());
    assert_eq!(fs::read(&icon_file).unwrap(), sample_png_bytes());
}

#[test]
fn test_tier4_complete_end_to_end_cli_workflow() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("workflow_app.apk");

    let spec = ApkSpec {
        package_name: "org.rustydroid.e2eworkflow".to_string(),
        app_label: "E2E Workflow App".to_string(),
        version_name: "3.0.0".to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    // Step 1: Status check
    let (s_status, s_out, _) = sandbox.run_cli(&["status"]).unwrap();
    assert!(s_status.success());
    assert!(s_out.contains("Linux Kernel:"));

    // Step 2: Init
    let (i_status, i_out, _) = sandbox.run_cli(&["init"]).unwrap();
    assert!(i_status.success());
    assert!(i_out.contains("Storage tree prepared at:"));
    assert!(sandbox.data_dir().join("rootfs").exists());

    // Step 3: Info
    let (inf_status, inf_out, _) = sandbox.run_cli(&["info", &apk_path.to_string_lossy()]).unwrap();
    assert!(inf_status.success());
    assert!(inf_out.contains("org.rustydroid.e2eworkflow"));

    // Step 4: Install
    let (ins_status, ins_out, _) = sandbox.run_cli(&["install", &apk_path.to_string_lossy()]).unwrap();
    assert!(ins_status.success());
    let desktop_file = sandbox.apps_dir().join("rustydroid-org.rustydroid.e2eworkflow.desktop");
    assert!(desktop_file.exists());

    // Step 5: Run
    let (r_status, r_out, _) = sandbox.run_cli(&["run", &apk_path.to_string_lossy()]).unwrap();
    assert!(r_status.success());
    assert!(r_out.contains("Inspecting APK:"));

    // Step 6: Stop
    let (stp_status, stp_out, _) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(stp_status.success());
    assert!(stp_out.contains("Stopping") || stp_out.contains("No active session"));
}
