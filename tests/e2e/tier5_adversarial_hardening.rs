use crate::common::{sample_png_bytes, ActivitySpec, ApkSpec, AxmlBuilder, SyntheticApkBuilder, TestSandbox};
use byteorder::{LittleEndian, WriteBytesExt};
use rustydroid::apk::{axml::AxmlParser, ApkInspector, DesktopIntegration};
use rustydroid::runtime::{SessionManager, SessionState};
use std::fs::{self, File};
use std::io::Write;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_tier5_adversarial_axml_chunk_overflow() {
    let mut buf = Vec::new();
    let total_size = 32;
    buf.write_u32::<LittleEndian>(0x00080003).unwrap(); // Header
    buf.write_u32::<LittleEndian>(total_size as u32).unwrap();

    // Chunk with size 999999 (exceeding file buffer)
    buf.write_u32::<LittleEndian>(0x00010001).unwrap();
    buf.write_u32::<LittleEndian>(999999).unwrap();
    buf.resize(total_size, 0);

    let mut parser = AxmlParser::new(&buf);
    let res = parser.parse();
    assert!(res.is_err(), "Exceeded chunk size must break parsing cleanly");
}

#[test]
fn test_tier5_adversarial_axml_unsupported_chunk_types() {
    let spec = ApkSpec::default();
    let original_axml = AxmlBuilder::build_from_spec(&spec);

    // Inject an unknown chunk (type 0x0999, size 16) between header and body
    let mut modified = Vec::new();
    let orig_len = original_axml.len();
    let new_len = orig_len + 16;

    modified.write_u32::<LittleEndian>(0x00080003).unwrap();
    modified.write_u32::<LittleEndian>(new_len as u32).unwrap();

    // Unknown chunk
    modified.write_u32::<LittleEndian>(0x00080999).unwrap(); // type 0x0999
    modified.write_u32::<LittleEndian>(16).unwrap(); // size 16
    modified.write_u32::<LittleEndian>(0x12345678).unwrap();
    modified.write_u32::<LittleEndian>(0x9abcdef0).unwrap();

    // Append remainder of original (excluding 8-byte header)
    modified.extend_from_slice(&original_axml[8..]);

    let mut parser = AxmlParser::new(&modified);
    let root = parser.parse().expect("Unknown chunk must be safely skipped by chunk_size");
    assert_eq!(root.name, "manifest");
}

#[test]
fn test_tier5_adversarial_special_characters_in_label() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("special_chars.apk");

    let special_label = "Evil'\"`\n\t;--App & <xml> 💥";
    let spec = ApkSpec {
        package_name: "org.rustydroid.special".to_string(),
        app_label: special_label.to_string(),
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_path, &spec).unwrap();

    let meta = ApkInspector::inspect(&apk_path).unwrap();
    let desktop_file = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid")).unwrap();
    assert!(desktop_file.exists());

    let content = fs::read_to_string(&desktop_file).unwrap();
    assert!(content.contains("[Desktop Entry]"));
    assert!(content.contains("StartupWMClass=org.rustydroid.special"));

    let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
}

#[test]
fn test_tier5_adversarial_empty_zip_entries() {
    let sandbox = TestSandbox::new().unwrap();
    let apk_path = sandbox.path().join("empty_entries.apk");

    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        // AndroidManifest.xml
        let spec = ApkSpec::default();
        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&AxmlBuilder::build_from_spec(&spec)).unwrap();

        // 0-byte directory entry
        zip.add_directory("res/", options).unwrap();
        zip.add_directory("res/drawable/", options).unwrap();

        // 0-byte file entry
        zip.start_file("res/drawable/empty.png", options).unwrap();

        // Valid icon
        zip.start_file("res/mipmap-hdpi-v4/ic_launcher.png", options).unwrap();
        zip.write_all(&sample_png_bytes()).unwrap();

        zip.finish().unwrap();
    }

    let meta = ApkInspector::inspect(&apk_path).expect("APK with empty zip entries should parse without crashing");
    assert!(meta.icon_data.is_some());
}

#[test]
fn test_tier5_adversarial_corrupted_session_json() {
    let sandbox = TestSandbox::new().unwrap();
    let session_file = sandbox.data_dir().join("session.json");

    fs::write(&session_file, b"{{INVALID_JSON_CORRUPTED: true").unwrap();
    assert!(session_file.exists());

    let res = serde_json::from_str::<SessionState>(&fs::read_to_string(&session_file).unwrap());
    assert!(res.is_err(), "Corrupted session json must return deserialization error, not panic");
}

#[test]
fn test_tier5_adversarial_subreaper_pid_boundaries() {
    let state_zero = SessionState {
        pid: 0,
        started_at: 0,
        display_mode: "".to_string(),
        is_active: false,
    };
    let json_zero = serde_json::to_string(&state_zero).unwrap();
    let decoded_zero: SessionState = serde_json::from_str(&json_zero).unwrap();
    assert_eq!(decoded_zero.pid, 0);

    let state_max = SessionState {
        pid: u32::MAX,
        started_at: u64::MAX,
        display_mode: "test".to_string(),
        is_active: true,
    };
    let json_max = serde_json::to_string(&state_max).unwrap();
    let decoded_max: SessionState = serde_json::from_str(&json_max).unwrap();
    assert_eq!(decoded_max.pid, u32::MAX);
}
