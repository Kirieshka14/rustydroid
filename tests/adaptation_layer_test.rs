mod common;

use common::sandbox::TestSandbox;
use common::synthetic_apk::{ApkSpec, SyntheticApkBuilder};
use rustydroid::adb::client::{AdbClient, InstallOptions};
use rustydroid::apk::compat::{
    classify_split_tag, merge_prop_package_list, resolve_window_dimensions, select_install_splits,
    stage_bundle_artifacts, AbiStatus, AdaptationProfile, ApkBundleAnalyzer, BundleFormat,
    ProfileOverrides, SplitKind, WindowMode,
};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn extra(entries: &[(&str, &[u8])]) -> HashMap<String, Vec<u8>> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.to_vec()))
        .collect()
}

fn build_bundle_archive<F>(path: &Path, build_fn: F)
where
    F: FnOnce(&mut ZipWriter<File>, SimpleFileOptions),
{
    let file = File::create(path).expect("create bundle file");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    build_fn(&mut zip, options);
    zip.finish().expect("finish bundle zip");
}

#[test]
fn test_single_apk_abi_and_engine_detection() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Pure DEX APK (no lib/*.so)
    let pure_apk = dir.path().join("pure.apk");
    SyntheticApkBuilder::create_apk(
        &pure_apk,
        &ApkSpec {
            package_name: "org.rustydroid.puredex".to_string(),
            app_label: "Pure Dex".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let pure_insp = ApkBundleAnalyzer::inspect(&pure_apk, &["x86_64".into(), "x86".into()]).unwrap();
    assert_eq!(pure_insp.compat.format, BundleFormat::SingleApk);
    assert_eq!(pure_insp.compat.abi_status, AbiStatus::PureDex);
    assert!(pure_insp.compat.native_abis.is_empty());

    // 2. ARM64 Unity game APK
    let arm_apk = dir.path().join("unity_arm64.apk");
    SyntheticApkBuilder::create_apk(
        &arm_apk,
        &ApkSpec {
            package_name: "org.rustydroid.unitygame".to_string(),
            app_label: "Unity ARM Game".to_string(),
            extra_files: extra(&[
                ("lib/arm64-v8a/libunity.so", b"\x7fELFarm64"),
                ("assets/bin/Data/globalgamemanagers", b"unity"),
            ]),
            ..Default::default()
        },
    )
    .unwrap();

    // Without ARM translation in container
    let no_arm_insp = ApkBundleAnalyzer::inspect(&arm_apk, &["x86_64".into(), "x86".into()]).unwrap();
    assert_eq!(
        no_arm_insp.compat.abi_status,
        AbiStatus::ArmTranslationRequired {
            abis: vec!["arm64-v8a".to_string()],
            container_supports_arm: false,
        }
    );
    assert_eq!(no_arm_insp.compat.game_engine.as_deref(), Some("Unity"));
    assert!(no_arm_insp.compat.is_game);
    assert!(!no_arm_insp.compat.warnings.is_empty());

    // With ARM translation enabled in container
    let arm_ready_insp = ApkBundleAnalyzer::inspect(
        &arm_apk,
        &[
            "x86_64".into(),
            "x86".into(),
            "arm64-v8a".into(),
            "armeabi-v7a".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        arm_ready_insp.compat.abi_status,
        AbiStatus::ArmTranslationRequired {
            abis: vec!["arm64-v8a".to_string()],
            container_supports_arm: true,
        }
    );
    assert!(arm_ready_insp.recommended_profile.fake_touch);
    assert_eq!(
        arm_ready_insp.recommended_profile.fake_wifi,
        arm_ready_insp.compat.uses_network_state
    );

    // 3. Dual-ABI APK (x86_64 + arm64-v8a) -> NativeX86
    let dual_apk = dir.path().join("dual.apk");
    SyntheticApkBuilder::create_apk(
        &dual_apk,
        &ApkSpec {
            package_name: "org.rustydroid.dual".to_string(),
            app_label: "Dual Native".to_string(),
            extra_files: extra(&[
                ("lib/arm64-v8a/libcore.so", b"\x7fELFarm"),
                ("lib/x86_64/libcore.so", b"\x7fELFx86"),
            ]),
            ..Default::default()
        },
    )
    .unwrap();
    let dual_insp = ApkBundleAnalyzer::inspect(&dual_apk, &["x86_64".into(), "arm64-v8a".into()]).unwrap();
    assert!(matches!(dual_insp.compat.abi_status, AbiStatus::NativeX86 { .. }));
}

#[test]
fn test_xapk_bundle_inspection_split_selection_and_staging() {
    let dir = tempfile::tempdir().unwrap();
    let base_apk_path = dir.path().join("inner_base.apk");
    SyntheticApkBuilder::create_apk(
        &base_apk_path,
        &ApkSpec {
            package_name: "com.example.xapkgame".to_string(),
            app_label: "XAPK Game".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let base_bytes = fs::read(&base_apk_path).unwrap();

    let arm_split_path = dir.path().join("inner_arm.apk");
    SyntheticApkBuilder::create_apk(
        &arm_split_path,
        &ApkSpec {
            package_name: "com.example.xapkgame".to_string(),
            extra_files: extra(&[("lib/arm64-v8a/libgame.so", b"\x7fELFarm")]),
            ..Default::default()
        },
    )
    .unwrap();
    let arm_bytes = fs::read(&arm_split_path).unwrap();

    let x86_split_path = dir.path().join("inner_x86.apk");
    SyntheticApkBuilder::create_apk(
        &x86_split_path,
        &ApkSpec {
            package_name: "com.example.xapkgame".to_string(),
            extra_files: extra(&[("lib/x86_64/libgame.so", b"\x7fELFx86")]),
            ..Default::default()
        },
    )
    .unwrap();
    let x86_bytes = fs::read(&x86_split_path).unwrap();

    let xapk_path = dir.path().join("game_bundle.xapk");
    let manifest_json = serde_json::json!({
        "xapk_version": 2,
        "package_name": "com.example.xapkgame",
        "name": "XAPK Game",
        "version_code": "42",
        "version_name": "1.2.3",
        "split_apks": [
            {"file": "com.example.xapkgame.apk", "id": "base"},
            {"file": "config.arm64_v8a.apk", "id": "config.arm64_v8a"},
            {"file": "config.x86_64.apk", "id": "config.x86_64"},
            {"file": "config.xxhdpi.apk", "id": "config.xxhdpi"},
            {"file": "config.ru.apk", "id": "config.ru"},
            {"file": "config.en.apk", "id": "config.en"}
        ],
        "expansions": [
            {
                "file": "Android/obb/com.example.xapkgame/main.42.com.example.xapkgame.obb",
                "install_location": "EXTERNAL_STORAGE",
                "install_path": "Android/obb/com.example.xapkgame/main.42.com.example.xapkgame.obb"
            }
        ]
    });

    build_bundle_archive(&xapk_path, |zip, opts| {
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(serde_json::to_string_pretty(&manifest_json).unwrap().as_bytes())
            .unwrap();

        zip.start_file("com.example.xapkgame.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();

        zip.start_file("config.arm64_v8a.apk", opts).unwrap();
        zip.write_all(&arm_bytes).unwrap();

        zip.start_file("config.x86_64.apk", opts).unwrap();
        zip.write_all(&x86_bytes).unwrap();

        zip.start_file("config.xxhdpi.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();

        zip.start_file("config.ru.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();

        zip.start_file("config.en.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();

        zip.start_file(
            "Android/obb/com.example.xapkgame/main.42.com.example.xapkgame.obb",
            opts,
        )
        .unwrap();
        zip.write_all(b"OBB_PAYLOAD_DATA").unwrap();
    });

    let container_abis = vec![
        "x86_64".to_string(),
        "x86".to_string(),
        "arm64-v8a".to_string(),
        "armeabi-v7a".to_string(),
    ];
    let insp = ApkBundleAnalyzer::inspect(&xapk_path, &container_abis).unwrap();
    assert_eq!(insp.compat.format, BundleFormat::Xapk);
    assert_eq!(insp.metadata.package_name, "com.example.xapkgame");
    assert_eq!(insp.compat.obb_files.len(), 1);
    assert_eq!(
        insp.compat.obb_files[0].file_name,
        "main.42.com.example.xapkgame.obb"
    );

    // Prefers native x86_64 when container lists x86_64 first
    let auto_splits = select_install_splits(&insp, &container_abis, None, Some("ru"));
    assert!(auto_splits.contains(&"com.example.xapkgame.apk".to_string()));
    assert!(auto_splits.contains(&"config.x86_64.apk".to_string()));
    assert!(!auto_splits.contains(&"config.arm64_v8a.apk".to_string()));
    assert!(auto_splits.contains(&"config.ru.apk".to_string()));

    // Respects explicit arm64-v8a ABI override
    let forced_arm_splits =
        select_install_splits(&insp, &container_abis, Some("arm64-v8a"), Some("en"));
    assert!(forced_arm_splits.contains(&"config.arm64_v8a.apk".to_string()));
    assert!(!forced_arm_splits.contains(&"config.x86_64.apk".to_string()));

    // Stages APK splits and OBB payload
    let staging_dir = dir.path().join("stage");
    let plan = stage_bundle_artifacts(&insp, &forced_arm_splits, &staging_dir).unwrap();
    assert_eq!(plan.apk_paths.len(), forced_arm_splits.len());
    for p in &plan.apk_paths {
        assert!(p.exists(), "staged APK missing: {:?}", p);
    }
    assert_eq!(plan.obb_files.len(), 1);
    let (host_obb, remote_obb) = &plan.obb_files[0];
    assert!(host_obb.exists());
    assert_eq!(fs::read(host_obb).unwrap(), b"OBB_PAYLOAD_DATA");
    assert_eq!(
        remote_obb,
        "/sdcard/Android/obb/com.example.xapkgame/main.42.com.example.xapkgame.obb"
    );
}

#[test]
fn test_apks_and_apkm_bundle_formats() {
    let dir = tempfile::tempdir().unwrap();
    let base_apk_path = dir.path().join("base.apk");
    SyntheticApkBuilder::create_apk(
        &base_apk_path,
        &ApkSpec {
            package_name: "org.rustydroid.bundle".to_string(),
            app_label: "Split Bundle".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let base_bytes = fs::read(&base_apk_path).unwrap();

    // 1. .apks archive (bundletool style)
    let apks_path = dir.path().join("app.apks");
    build_bundle_archive(&apks_path, |zip, opts| {
        zip.start_file("splits/base-master.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();
        zip.start_file("splits/base-arm64_v8a.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();
        zip.start_file("splits/base-en.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();
    });
    let apks_insp =
        ApkBundleAnalyzer::inspect(&apks_path, &["x86_64".into(), "arm64-v8a".into()]).unwrap();
    assert_eq!(apks_insp.compat.format, BundleFormat::Apks);
    assert_eq!(apks_insp.metadata.package_name, "org.rustydroid.bundle");

    // 2. .apkm archive (APKMirror info.json style)
    let apkm_path = dir.path().join("app.apkm");
    let info_json = r#"{"pname":"org.rustydroid.bundle","app_name":"Split Bundle","versioncode":"1"}"#;
    build_bundle_archive(&apkm_path, |zip, opts| {
        zip.start_file("info.json", opts).unwrap();
        zip.write_all(info_json.as_bytes()).unwrap();
        zip.start_file("base.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();
        zip.start_file("split_config.arm64_v8a.apk", opts).unwrap();
        zip.write_all(&base_bytes).unwrap();
    });
    let apkm_insp = ApkBundleAnalyzer::inspect(&apkm_path, &["arm64-v8a".into()]).unwrap();
    assert_eq!(apkm_insp.compat.format, BundleFormat::Apkm);
    assert_eq!(
        classify_split_tag("splits/base-arm64_v8a.apk"),
        SplitKind::Abi("arm64-v8a".to_string())
    );
}

#[test]
fn test_adb_install_args_and_window_scaling_math() {
    let mut profile = AdaptationProfile {
        package_name: "org.rustydroid.app".to_string(),
        window_mode: WindowMode::PhonePortrait,
        width: None,
        height: None,
        lcd_density: Some(320),
        fake_wifi: true,
        fake_touch: true,
        auto_grant_permissions: true,
        installer_spoof: Some("com.android.vending".to_string()),
        abi_override: Some("arm64-v8a".to_string()),
        allow_test_only: true,
        allow_downgrade: true,
        force_sdk: true,
        ..Default::default()
    };

    let opts = InstallOptions::from_profile(&profile);
    let args = AdbClient::build_adb_install_args(
        "192.168.240.112:5555",
        &[PathBuf::from("/tmp/base.apk"), PathBuf::from("/tmp/split.apk")],
        &opts,
    );
    assert_eq!(args[0], "-s");
    assert_eq!(args[1], "192.168.240.112:5555");
    assert_eq!(args[2], "install-multiple");
    assert!(args.contains(&"-r".to_string()));
    assert!(args.contains(&"-t".to_string()));
    assert!(args.contains(&"-d".to_string()));
    assert!(args.contains(&"-g".to_string()));
    assert!(args.contains(&"com.android.vending".to_string()));
    assert!(args.contains(&"arm64-v8a".to_string()));
    assert!(args.contains(&"--force-sdk".to_string()));

    // Check window bounds clamping against 1324x688 Wayland output
    let portrait = resolve_window_dimensions(&profile, 1324, 688).unwrap();
    assert!(portrait.0 <= 1324 && portrait.1 <= 688);
    assert!(portrait.0 < portrait.1);

    profile.apply_overrides(&ProfileOverrides {
        window_mode: Some(WindowMode::CustomSize),
        width: Some(2560),
        height: Some(1440),
        ..Default::default()
    });
    let custom = resolve_window_dimensions(&profile, 1324, 688).unwrap();
    assert!(custom.0 <= 1324 && custom.1 <= 688);

    // Check 91-byte Waydroid property limit CSV helper
    let merged = merge_prop_package_list(
        "com.alpha.one,com.beta.two,com.gamma.three,com.delta.four,com.epsilon.five,com.zeta.six",
        "org.rustydroid.app",
        true,
        91,
    );
    assert!(merged.len() <= 91);
    assert!(merged.starts_with("org.rustydroid.app"));
}

#[test]
fn test_ensure_waydroid_adb_authorized_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let adb_dir = dir.path().join("misc/adb");
    fs::create_dir_all(&adb_dir).unwrap();
    AdbClient::ensure_waydroid_adb_authorized(dir.path()).unwrap();
    let first = fs::read_to_string(adb_dir.join("adb_keys")).unwrap_or_default();
    AdbClient::ensure_waydroid_adb_authorized(dir.path()).unwrap();
    let second = fs::read_to_string(adb_dir.join("adb_keys")).unwrap_or_default();
    assert_eq!(first, second);
}

#[test]
fn test_cli_adapt_workflow_and_bundle_sandbox_e2e() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("adapt_demo.apk");
    SyntheticApkBuilder::create_apk(
        &apk_path,
        &ApkSpec {
            package_name: "org.rustydroid.adaptcli".to_string(),
            app_label: "Adapt CLI App".to_string(),
            extra_files: extra(&[("lib/arm64-v8a/libdemo.so", b"\x7fELFarm")]),
            ..Default::default()
        },
    )
    .unwrap();
    let apk_str = apk_path.to_string_lossy().to_string();

    // 1. info shows adaptation analysis + ARM requirement
    let (info_status, info_out, _) = sandbox.run_cli(&["info", &apk_str]).unwrap();
    assert!(info_status.success());
    assert!(info_out.contains("org.rustydroid.adaptcli"));
    assert!(info_out.contains("arm64-v8a"));

    // 2. adapt set saves per-app profile
    let (set_status, set_out, set_err) = sandbox
        .run_cli(&[
            "adapt",
            "set",
            &apk_str,
            "--window-mode",
            "portrait",
            "--fake-wifi",
            "--fake-touch",
            "--dpi",
            "280",
            "--abi",
            "arm64-v8a",
        ])
        .unwrap();
    assert!(set_status.success(), "adapt set failed: {}", set_err);
    assert!(set_out.contains("org.rustydroid.adaptcli"));

    // 3. adapt show reflects saved flags
    let (show_status, show_out, _) = sandbox
        .run_cli(&["adapt", "show", "org.rustydroid.adaptcli"])
        .unwrap();
    assert!(show_status.success());
    assert!(show_out.contains("PhonePortrait"));
    assert!(show_out.contains("fake_wifi:") && show_out.contains("true"));
    assert!(show_out.contains("fake_touch:"));
    assert!(show_out.contains("280 dpi"));

    // 4. adapt list shows the saved profile
    let (list_status, list_out, _) = sandbox.run_cli(&["adapt", "list"]).unwrap();
    assert!(list_status.success());
    assert!(list_out.contains("org.rustydroid.adaptcli"));

    // 5. install with flags saves desktop entry + profile in sandbox
    let (inst_status, inst_out, inst_err) = sandbox
        .run_cli(&[
            "install",
            &apk_str,
            "--window-mode",
            "landscape",
            "--allow-downgrade",
        ])
        .unwrap();
    assert!(inst_status.success(), "install failed: {}", inst_err);
    assert!(inst_out.contains("Desktop entry registered:"));

    // 6. run APK in headless sandbox completes cleanly and starts session bookkeeping
    let (run_status, run_out, run_err) = sandbox.run_cli(&["run", &apk_str]).unwrap();
    assert!(run_status.success(), "run failed: {}", run_err);
    assert!(run_out.contains("Inspecting APK:"));

    // 7. run and inspect an .xapk bundle via CLI in sandbox
    let apk_bytes = fs::read(&apk_path).unwrap();
    let xapk_path = sandbox.path().join("adapt_demo.xapk");
    build_bundle_archive(&xapk_path, |zip, opts| {
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(
            br#"{"package_name":"org.rustydroid.adaptcli","split_apks":[{"file":"base.apk","id":"base"}]}"#,
        )
        .unwrap();
        zip.start_file("base.apk", opts).unwrap();
        zip.write_all(&apk_bytes).unwrap();
    });
    let xapk_str = xapk_path.to_string_lossy().to_string();
    let (xinfo_status, xinfo_out, xinfo_err) = sandbox.run_cli(&["info", &xapk_str]).unwrap();
    assert!(xinfo_status.success(), "xapk info failed: {}", xinfo_err);
    assert!(xinfo_out.contains("XAPK"));
    let (xrun_status, xrun_out, xrun_err) = sandbox.run_cli(&["run", &xapk_str]).unwrap();
    assert!(xrun_status.success(), "xapk run failed: {}", xrun_err);
    assert!(xrun_out.contains("Inspecting APK:"));

    // 8. adapt reset deletes profile
    let (reset_status, reset_out, _) = sandbox
        .run_cli(&["adapt", "reset", "org.rustydroid.adaptcli"])
        .unwrap();
    assert!(reset_status.success());
    assert!(reset_out.contains("удалён"));

    // Clean up supervisor daemon started by headless `run`
    let (stop_status, _, _) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(stop_status.success());
}
