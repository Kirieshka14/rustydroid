#![allow(dead_code, unused_imports)]

use byteorder::{LittleEndian, WriteBytesExt};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[derive(Debug, Clone)]
pub struct ActivitySpec {
    pub name: String,
    pub is_launcher: bool,
    pub is_alias: bool,
}

#[derive(Debug, Clone)]
pub struct ApkSpec {
    pub package_name: String,
    pub version_code: u32,
    pub version_name: String,
    pub app_label: String,
    pub permissions: Vec<String>,
    pub activities: Vec<ActivitySpec>,
    pub use_utf8_string_pool: bool,
    pub include_resource_map: bool,
    pub include_namespaces: bool,
    pub icon_path: Option<String>,
    pub icon_bytes: Option<Vec<u8>>,
    pub extra_files: HashMap<String, Vec<u8>>,
}

impl Default for ApkSpec {
    fn default() -> Self {
        Self {
            package_name: "org.rustydroid.sample".to_string(),
            version_code: 100,
            version_name: "1.0.0".to_string(),
            app_label: "RustyDroid Sample".to_string(),
            permissions: vec![
                "android.permission.INTERNET".to_string(),
                "android.permission.ACCESS_NETWORK_STATE".to_string(),
            ],
            activities: vec![ActivitySpec {
                name: ".MainActivity".to_string(),
                is_launcher: true,
                is_alias: false,
            }],
            use_utf8_string_pool: false,
            include_resource_map: false,
            include_namespaces: false,
            icon_path: Some("res/mipmap-xxxhdpi-v4/ic_launcher.png".to_string()),
            icon_bytes: Some(sample_png_bytes()),
            extra_files: HashMap::new(),
        }
    }
}

pub fn sample_png_bytes() -> Vec<u8> {
    // Valid PNG signature + minimal chunk structure
    let mut v = Vec::new();
    v.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    // IHDR chunk: 13 bytes
    v.write_u32::<LittleEndian>(13).unwrap();
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&[0; 13]);
    v.write_u32::<LittleEndian>(0).unwrap(); // CRC
    // IEND chunk: 0 bytes
    v.write_u32::<LittleEndian>(0).unwrap();
    v.extend_from_slice(b"IEND");
    v.write_u32::<LittleEndian>(0xae426082).unwrap(); // CRC
    v
}

pub struct AxmlBuilder {
    strings: Vec<String>,
    string_map: HashMap<String, u32>,
    use_utf8: bool,
    include_resource_map: bool,
    include_namespaces: bool,
}

impl AxmlBuilder {
    pub fn new(use_utf8: bool) -> Self {
        Self {
            strings: Vec::new(),
            string_map: HashMap::new(),
            use_utf8,
            include_resource_map: false,
            include_namespaces: false,
        }
    }

    pub fn with_resource_map(mut self, enabled: bool) -> Self {
        self.include_resource_map = enabled;
        self
    }

    pub fn with_namespaces(mut self, enabled: bool) -> Self {
        self.include_namespaces = enabled;
        self
    }

    pub fn add_string(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.string_map.get(s) {
            idx
        } else {
            let idx = self.strings.len() as u32;
            self.strings.push(s.to_string());
            self.string_map.insert(s.to_string(), idx);
            idx
        }
    }

    pub fn build_string_pool(&self) -> (Vec<u8>, Vec<u32>) {
        let mut sp_buf = Vec::new();
        let mut str_offsets = Vec::new();
        let mut str_bytes_buf = Vec::new();

        for s in &self.strings {
            str_offsets.push(str_bytes_buf.len() as u32);
            if self.use_utf8 {
                let char_len = s.chars().count();
                let byte_len = s.len();

                // Encode character count
                if char_len > 127 {
                    str_bytes_buf.push((0x80 | ((char_len >> 8) & 0x7F)) as u8);
                    str_bytes_buf.push((char_len & 0xFF) as u8);
                } else {
                    str_bytes_buf.push(char_len as u8);
                }

                // Encode byte count
                if byte_len > 127 {
                    str_bytes_buf.push((0x80 | ((byte_len >> 8) & 0x7F)) as u8);
                    str_bytes_buf.push((byte_len & 0xFF) as u8);
                } else {
                    str_bytes_buf.push(byte_len as u8);
                }

                str_bytes_buf.extend_from_slice(s.as_bytes());
                str_bytes_buf.push(0); // null terminator
            } else {
                let u16s: Vec<u16> = s.encode_utf16().collect();
                let len = u16s.len();
                if len >= 0x8000 {
                    let w1 = ((len >> 16) | 0x8000) as u16;
                    let w2 = (len & 0xFFFF) as u16;
                    str_bytes_buf.write_u16::<LittleEndian>(w1).unwrap();
                    str_bytes_buf.write_u16::<LittleEndian>(w2).unwrap();
                } else {
                    str_bytes_buf.write_u16::<LittleEndian>(len as u16).unwrap();
                }

                for ch in u16s {
                    str_bytes_buf.write_u16::<LittleEndian>(ch).unwrap();
                }
                str_bytes_buf.write_u16::<LittleEndian>(0).unwrap(); // null terminator
            }
        }

        let sp_header_size = 28 + (self.strings.len() * 4);
        let sp_size = sp_header_size + str_bytes_buf.len();

        sp_buf.write_u32::<LittleEndian>(0x00010001).unwrap(); // CHUNK_STRING_POOL
        sp_buf.write_u32::<LittleEndian>(sp_size as u32).unwrap();
        sp_buf.write_u32::<LittleEndian>(self.strings.len() as u32).unwrap();
        sp_buf.write_u32::<LittleEndian>(0).unwrap(); // style count
        let flags = if self.use_utf8 { 1 << 8 } else { 0 };
        sp_buf.write_u32::<LittleEndian>(flags).unwrap();
        sp_buf.write_u32::<LittleEndian>(sp_header_size as u32).unwrap();
        sp_buf.write_u32::<LittleEndian>(0).unwrap(); // styles start

        for off in &str_offsets {
            sp_buf.write_u32::<LittleEndian>(*off).unwrap();
        }
        sp_buf.extend_from_slice(&str_bytes_buf);

        (sp_buf, str_offsets)
    }

    pub fn build_from_spec(spec: &ApkSpec) -> Vec<u8> {
        let mut builder = AxmlBuilder::new(spec.use_utf8_string_pool)
            .with_resource_map(spec.include_resource_map)
            .with_namespaces(spec.include_namespaces);

        // Pre-populate strings
        let s_manifest = builder.add_string("manifest");
        let s_package = builder.add_string("package");
        let s_package_val = builder.add_string(&spec.package_name);
        let s_version_code = builder.add_string("versionCode");
        let s_version_name = builder.add_string("versionName");
        let s_version_name_val = builder.add_string(&spec.version_name);

        let s_application = builder.add_string("application");
        let s_label = builder.add_string("label");
        let s_label_val = builder.add_string(&spec.app_label);

        let s_activity = builder.add_string("activity");
        let s_activity_alias = builder.add_string("activity-alias");
        let s_name = builder.add_string("name");
        let s_intent_filter = builder.add_string("intent-filter");
        let s_action = builder.add_string("action");
        let s_action_main = builder.add_string("android.intent.action.MAIN");
        let s_category = builder.add_string("category");
        let s_category_launcher = builder.add_string("android.intent.category.LAUNCHER");

        let s_uses_permission = builder.add_string("uses-permission");

        let mut perm_indices = Vec::new();
        for p in &spec.permissions {
            perm_indices.push(builder.add_string(p));
        }

        let mut activity_indices = Vec::new();
        for act in &spec.activities {
            activity_indices.push((builder.add_string(&act.name), act.is_launcher, act.is_alias));
        }

        let (sp_buf, _) = builder.build_string_pool();

        let mut tags_buf = Vec::new();

        // 1. Optional Resource Map Chunk (0x0180)
        if builder.include_resource_map {
            let res_map_size = 8 + 4 * 3;
            tags_buf.write_u32::<LittleEndian>(0x00080180).unwrap();
            tags_buf.write_u32::<LittleEndian>(res_map_size as u32).unwrap();
            tags_buf.write_u32::<LittleEndian>(0x01010001).unwrap();
            tags_buf.write_u32::<LittleEndian>(0x01010002).unwrap();
            tags_buf.write_u32::<LittleEndian>(0x01010003).unwrap();
        }

        // 2. Optional Namespace Start Chunk (0x0100)
        if builder.include_namespaces {
            tags_buf.write_u32::<LittleEndian>(0x00100100).unwrap();
            tags_buf.write_u32::<LittleEndian>(24).unwrap();
            tags_buf.write_u32::<LittleEndian>(1).unwrap();
            tags_buf.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            tags_buf.write_u32::<LittleEndian>(0).unwrap(); // prefix
            tags_buf.write_u32::<LittleEndian>(1).unwrap(); // uri
        }

        let write_start_tag = |b: &mut Vec<u8>, name_idx: u32, attrs: &[(u32, u32, u32, u32, u32)]| {
            let size = 36 + (attrs.len() * 20);
            b.write_u32::<LittleEndian>(0x00100102).unwrap(); // CHUNK_START_TAG
            b.write_u32::<LittleEndian>(size as u32).unwrap();
            b.write_u32::<LittleEndian>(1).unwrap();
            b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            b.write_u32::<LittleEndian>(name_idx).unwrap();
            b.write_u16::<LittleEndian>(0x0014).unwrap();
            b.write_u16::<LittleEndian>(0x0014).unwrap();
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
        };

        let write_end_tag = |b: &mut Vec<u8>, name_idx: u32| {
            b.write_u32::<LittleEndian>(0x00100103).unwrap(); // CHUNK_END_TAG
            b.write_u32::<LittleEndian>(24).unwrap();
            b.write_u32::<LittleEndian>(1).unwrap();
            b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            b.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            b.write_u32::<LittleEndian>(name_idx).unwrap();
        };

        // <manifest package="..." versionCode=... versionName="...">
        write_start_tag(
            &mut tags_buf,
            s_manifest,
            &[
                (0xFFFFFFFF, s_package, s_package_val, 0x03, s_package_val),
                (0xFFFFFFFF, s_version_code, 0xFFFFFFFF, 0x10, spec.version_code),
                (0xFFFFFFFF, s_version_name, s_version_name_val, 0x03, s_version_name_val),
            ],
        );

        // <uses-permission name="..." />
        for perm_idx in perm_indices {
            write_start_tag(&mut tags_buf, s_uses_permission, &[(0xFFFFFFFF, s_name, perm_idx, 0x03, perm_idx)]);
            write_end_tag(&mut tags_buf, s_uses_permission);
        }

        // <application label="...">
        write_start_tag(&mut tags_buf, s_application, &[(0xFFFFFFFF, s_label, s_label_val, 0x03, s_label_val)]);

        // Activities
        for (act_name_idx, is_launcher, is_alias) in activity_indices {
            let tag = if is_alias { s_activity_alias } else { s_activity };
            write_start_tag(&mut tags_buf, tag, &[(0xFFFFFFFF, s_name, act_name_idx, 0x03, act_name_idx)]);

            if is_launcher {
                // <intent-filter>
                write_start_tag(&mut tags_buf, s_intent_filter, &[]);
                // <action name="android.intent.action.MAIN" />
                write_start_tag(&mut tags_buf, s_action, &[(0xFFFFFFFF, s_name, s_action_main, 0x03, s_action_main)]);
                write_end_tag(&mut tags_buf, s_action);
                // <category name="android.intent.category.LAUNCHER" />
                write_start_tag(&mut tags_buf, s_category, &[(0xFFFFFFFF, s_name, s_category_launcher, 0x03, s_category_launcher)]);
                write_end_tag(&mut tags_buf, s_category);
                // </intent-filter>
                write_end_tag(&mut tags_buf, s_intent_filter);
            }

            write_end_tag(&mut tags_buf, tag);
        }

        // Close <application>
        write_end_tag(&mut tags_buf, s_application);
        // Close <manifest>
        write_end_tag(&mut tags_buf, s_manifest);

        // 3. Optional Namespace End Chunk (0x0101)
        if builder.include_namespaces {
            tags_buf.write_u32::<LittleEndian>(0x00100101).unwrap();
            tags_buf.write_u32::<LittleEndian>(24).unwrap();
            tags_buf.write_u32::<LittleEndian>(1).unwrap();
            tags_buf.write_u32::<LittleEndian>(0xFFFFFFFF).unwrap();
            tags_buf.write_u32::<LittleEndian>(0).unwrap();
            tags_buf.write_u32::<LittleEndian>(1).unwrap();
        }

        let total_size = 8 + sp_buf.len() + tags_buf.len();
        let mut full = Vec::with_capacity(total_size);
        full.write_u32::<LittleEndian>(0x00080003).unwrap(); // MAGIC + HEADER_SIZE
        full.write_u32::<LittleEndian>(total_size as u32).unwrap();
        full.extend_from_slice(&sp_buf);
        full.extend_from_slice(&tags_buf);

        full
    }
}

pub struct SyntheticApkBuilder;

impl SyntheticApkBuilder {
    pub fn create_apk<P: AsRef<Path>>(target_path: P, spec: &ApkSpec) -> anyhow::Result<PathBuf> {
        let path = target_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = File::create(&path)?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();

        // 1. AndroidManifest.xml
        zip.start_file("AndroidManifest.xml", options)?;
        let axml_bytes = AxmlBuilder::build_from_spec(spec);
        zip.write_all(&axml_bytes)?;

        // 2. Icon file if requested
        if let (Some(ref icon_path), Some(ref icon_bytes)) = (&spec.icon_path, &spec.icon_bytes) {
            zip.start_file(icon_path, options)?;
            zip.write_all(icon_bytes)?;
        }

        // 3. Extra files
        for (name, content) in &spec.extra_files {
            zip.start_file(name, options)?;
            zip.write_all(content)?;
        }

        zip.finish()?;
        Ok(path)
    }
}
