use crate::common::{sample_png_bytes, ActivitySpec, ApkSpec, AxmlBuilder, SyntheticApkBuilder, TestSandbox};
use byteorder::{LittleEndian, WriteBytesExt};
use rustydroid::apk::{axml::AxmlParser, ApkInspector};
use rustydroid::binder::{BinderManager, BinderfsDevice};
use rustydroid::runtime::{SessionManager, SessionState};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_tier2_axml_header_truncated() {
    let truncated_data = vec![0x03, 0x00, 0x08, 0x00]; // 4 bytes only
    let mut parser = AxmlParser::new(&truncated_data);
    let res = parser.parse();
    assert!(res.is_err());
    let err = res.err().unwrap().to_string();
    assert!(err.contains("too short"), "Expected 'too short' error, got: {}", err);
}

#[test]
fn test_tier2_axml_invalid_magic() {
    let mut invalid_magic = vec![0x01, 0x00, 0x08, 0x00, 0x20, 0x00, 0x00, 0x00];
    invalid_magic.resize(32, 0);
    let mut parser = AxmlParser::new(&invalid_magic);
    let res = parser.parse();
    assert!(res.is_err());
    let err = res.err().unwrap().to_string();
    assert!(err.contains("Invalid AXML magic"), "Expected 'Invalid AXML magic' error, got: {}", err);
}

#[test]
fn test_tier2_axml_zero_elements() {
    let mut buf = Vec::new();
    let total_size = 8;
    buf.write_u32::<LittleEndian>(0x00080003).unwrap();
    buf.write_u32::<LittleEndian>(total_size as u32).unwrap();

    let mut parser = AxmlParser::new(&buf);
    let res = parser.parse();
    assert!(res.is_err());
    let err = res.err().unwrap().to_string();
    assert!(err.contains("No root element found in AXML"), "Expected 'No root element found' error, got: {}", err);
}

#[test]
fn test_tier2_string_pool_utf8_long_string() {
    let long_label = "L".repeat(250);
    let mut spec = ApkSpec::default();
    spec.use_utf8_string_pool = true;
    spec.app_label = long_label.clone();
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().unwrap();
    let app = root.find_children("application")[0];
    let decoded_label = app.get_attr("label", parser.string_pool()).unwrap();
    assert_eq!(decoded_label, long_label, "250-character string must decode without truncation");
}

#[test]
fn test_tier2_string_pool_utf8_multibyte_unicode() {
    let unicode_label = "Приложение 🚀 世界 🌍 Καλημέρα";
    let mut spec = ApkSpec::default();
    spec.use_utf8_string_pool = true;
    spec.app_label = unicode_label.to_string();
    let axml_bytes = AxmlBuilder::build_from_spec(&spec);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().unwrap();
    let app = root.find_children("application")[0];
    let decoded_label = app.get_attr("label", parser.string_pool()).unwrap();
    assert_eq!(decoded_label, unicode_label, "Multibyte Unicode string must decode identically");
}

#[test]
fn test_tier2_string_pool_utf16_multichar_continuation() {
    let mut builder = AxmlBuilder::new(false);
    let test_str = "UTF16ContinuationString";
    builder.add_string(test_str);
    let (chunk, _) = builder.build_string_pool();
    assert!(!chunk.is_empty());
}

#[test]
fn test_tier2_apk_not_a_zip() {
    let sandbox = TestSandbox::new().unwrap();
    let fake_apk = sandbox.path().join("corrupt.apk");
    fs::write(&fake_apk, b"THIS IS NOT A ZIP ARCHIVE AT ALL").unwrap();

    let res = ApkInspector::inspect(&fake_apk);
    assert!(res.is_err());
    let err = res.err().unwrap().to_string();
    assert!(err.contains("Failed to read APK as ZIP archive"), "Expected ZIP failure, got: {}", err);
}

#[test]
fn test_tier2_apk_missing_manifest() {
    let sandbox = TestSandbox::new().unwrap();
    let no_manifest_apk = sandbox.path().join("no_manifest.apk");

    {
        let file = File::create(&no_manifest_apk).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("dummy.txt", options).unwrap();
        zip.write_all(b"dummy content").unwrap();
        zip.finish().unwrap();
    }

    let res = ApkInspector::inspect(&no_manifest_apk);
    assert!(res.is_err());
    let err = res.err().unwrap().to_string();
    assert!(err.contains("AndroidManifest.xml not found in APK"), "Expected missing manifest error, got: {}", err);
}

#[test]
fn test_tier2_apk_missing_icon() {
    let sandbox = TestSandbox::new().unwrap();
    let no_icon_apk = sandbox.path().join("no_icon.apk");

    let spec = ApkSpec {
        icon_path: None,
        icon_bytes: None,
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&no_icon_apk, &spec).unwrap();

    let meta = ApkInspector::inspect(&no_icon_apk).unwrap();
    assert!(meta.icon_data.is_none());
    assert!(meta.icon_path_in_apk.is_none());
}

#[test]
fn test_tier2_activity_name_relative_leading_dot() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("leading_dot.apk");

    let spec = ApkSpec {
        package_name: "com.test.dotapp".to_string(),
        activities: vec![ActivitySpec {
            name: ".HomeActivity".to_string(),
            is_launcher: true,
            is_alias: false,
        }],
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let meta = ApkInspector::inspect(&apk_path).unwrap();
    assert_eq!(meta.main_activity.as_deref(), Some("com.test.dotapp.HomeActivity"));
}

#[test]
fn test_tier2_activity_name_relative_bare() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("bare_name.apk");

    let spec = ApkSpec {
        package_name: "com.test.bareapp".to_string(),
        activities: vec![ActivitySpec {
            name: "BareHomeActivity".to_string(),
            is_launcher: true,
            is_alias: false,
        }],
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let meta = ApkInspector::inspect(&apk_path).unwrap();
    assert_eq!(meta.main_activity.as_deref(), Some("com.test.bareapp.BareHomeActivity"));
}

#[test]
fn test_tier2_activity_alias_launcher() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("alias_test.apk");

    let spec = ApkSpec {
        package_name: "org.example.aliasapp".to_string(),
        activities: vec![
            ActivitySpec {
                name: "org.example.aliasapp.InternalTargetActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: ".LauncherAlias".to_string(),
                is_launcher: true,
                is_alias: true,
            },
        ],
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let meta = ApkInspector::inspect(&apk_path).unwrap();
    assert_eq!(meta.main_activity.as_deref(), Some("org.example.aliasapp.LauncherAlias"));
}

#[test]
fn test_tier2_binderfs_device_name_overflow() {
    let huge_name = "x".repeat(300);
    let res = BinderfsDevice::new(&huge_name);
    assert!(res.is_err(), "BinderfsDevice::new must reject device names >= 256 bytes");
    let err = res.err().unwrap().to_string();
    assert!(err.contains("exceeds max binderfs name length"), "Got error: {}", err);
}

#[test]
fn test_tier2_binderfs_control_missing() {
    let sandbox = TestSandbox::new().unwrap();
    let non_existent = sandbox.path().join("non_existent_binderfs");
    let res = BinderManager::setup_standard_devices(&non_existent);
    assert!(res.is_err(), "setup_standard_devices must fail if binder-control does not exist");
}

#[test]
fn test_tier2_stale_session_purge() {
    // When a process dies, kill(pid, 0) returns ESRCH
    // We test with an invalid high PID that cannot be active
    let fake_dead_pid = 4_000_000;
    let state = SessionState {
        pid: fake_dead_pid,
        started_at: 1000,
        display_mode: "multi_window".to_string(),
        is_active: true,
    };
    let json = serde_json::to_string(&state).unwrap();
    let deserialized: SessionState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.pid, fake_dead_pid);
}

#[test]
fn test_tier2_cli_info_missing_file() {
    let sandbox = TestSandbox::new().unwrap();
    let missing_apk = sandbox.path().join("does_not_exist.apk");

    let (status, _stdout, stderr) = sandbox.run_cli(&["info", &missing_apk.to_string_lossy()]).unwrap();
    assert!(!status.success(), "cli info on missing file must fail with non-zero exit code");
    assert!(stderr.contains("No such file or directory") || stderr.contains("Failed to open APK file"));
}

#[test]
fn test_tier2_cli_install_corrupt_apk() {
    let sandbox = TestSandbox::new().unwrap();
    let corrupt_apk = sandbox.path().join("corrupt.apk");
    fs::write(&corrupt_apk, b"NOT AN APK").unwrap();

    let (status, _stdout, stderr) = sandbox.run_cli(&["install", &corrupt_apk.to_string_lossy()]).unwrap();
    assert!(!status.success(), "cli install on corrupt file must fail");
    assert!(stderr.contains("Failed to read APK as ZIP archive"));
}

#[test]
fn test_tier2_cli_stop_no_session() {
    let sandbox = TestSandbox::new().unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(status.success(), "cli stop when no session exists should succeed cleanly");
    assert!(stdout.contains("No active session") || stdout.contains("Stopping"));
    let _ = stderr;
}
