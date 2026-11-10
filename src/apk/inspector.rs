#![allow(dead_code)]

use super::axml::AxmlParser;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApkMetadata {
    pub file_path: String,
    pub package_name: String,
    pub version_code: Option<String>,
    pub version_name: Option<String>,
    pub app_label: Option<String>,
    pub main_activity: Option<String>,
    pub permissions: Vec<String>,
    pub icon_path_in_apk: Option<String>,
    #[serde(skip)]
    pub icon_data: Option<Vec<u8>>,
}

pub struct ApkInspector;

impl ApkInspector {
    pub fn normalize_activity_name(package_name: &str, act_name: &str) -> String {
        let trimmed = act_name.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.starts_with('.') {
            format!("{}{}", package_name, trimmed)
        } else if !trimmed.contains('.') {
            format!("{}.{}", package_name, trimmed)
        } else {
            trimmed.to_string()
        }
    }

    pub fn inspect<P: AsRef<Path>>(path: P) -> Result<ApkMetadata> {
        let path_ref = path.as_ref();
        let file = File::open(path_ref)
            .with_context(|| format!("Failed to open APK file: {}", path_ref.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| "Failed to read APK as ZIP archive")?;

        // 1. Read and parse AndroidManifest.xml
        let manifest_bytes = {
            let mut manifest_file = archive
                .by_name("AndroidManifest.xml")
                .context("AndroidManifest.xml not found in APK")?;
            let mut buf = Vec::new();
            manifest_file.read_to_end(&mut buf)?;
            buf
        };

        let mut parser = AxmlParser::new(&manifest_bytes);
        let root = parser.parse().context("Failed to parse AndroidManifest.xml AXML")?;
        let string_pool = parser.string_pool();

        let package_name = root
            .get_attr("package", string_pool)
            .unwrap_or_else(|| "unknown.package".to_string());
        let version_code = root.get_attr("versionCode", string_pool);
        let version_name = root.get_attr("versionName", string_pool);

        let mut app_label = None;
        let mut main_activity = None;
        let mut permissions = Vec::new();

        // Check uses-permission
        for child in root.find_children("uses-permission") {
            if let Some(perm) = child.get_attr("name", string_pool) {
                permissions.push(perm);
            }
        }

        // Check application tag
        for app in root.find_children("application") {
            if let Some(lbl) = app.get_attr("label", string_pool) {
                if !lbl.starts_with('@') {
                    app_label = Some(lbl);
                }
            }

            // Find activities and activity-alias
            for act in app.find_children("activity").into_iter().chain(app.find_children("activity-alias")) {
                let act_name = act.get_attr("name", string_pool)
                    .or_else(|| act.get_attr("targetActivity", string_pool));

                let mut has_launcher_filter = false;
                for filter in act.find_children("intent-filter") {
                    let mut is_main = false;
                    let mut is_launcher = false;

                    for action in filter.find_children("action") {
                        if let Some(act_action) = action.get_attr("name", string_pool) {
                            if act_action == "android.intent.action.MAIN" {
                                is_main = true;
                            }
                        }
                    }
                    for category in filter.find_children("category") {
                        if let Some(cat) = category.get_attr("name", string_pool) {
                            if cat == "android.intent.category.LAUNCHER" {
                                is_launcher = true;
                            }
                        }
                    }

                    if is_main && is_launcher {
                        has_launcher_filter = true;
                        break;
                    }
                }

                if has_launcher_filter && main_activity.is_none() {
                    if let Some(name) = act_name {
                        let full_activity = Self::normalize_activity_name(&package_name, &name);
                        if !full_activity.is_empty() {
                            main_activity = Some(full_activity);
                        }
                    }
                }
            }
        }

        // 2. Locate and extract app icon
        let mut icon_path_in_apk = None;
        let mut icon_data = None;

        // Candidate icon patterns ordered by preference (highest resolution first)
        let icon_candidates = [
            "res/mipmap-xxxhdpi-v4/ic_launcher.png",
            "res/mipmap-xxxhdpi/ic_launcher.png",
            "res/mipmap-xxhdpi-v4/ic_launcher.png",
            "res/mipmap-xxhdpi/ic_launcher.png",
            "res/mipmap-xhdpi-v4/ic_launcher.png",
            "res/mipmap-xhdpi/ic_launcher.png",
            "res/mipmap-hdpi-v4/ic_launcher.png",
            "res/mipmap-hdpi/ic_launcher.png",
            "res/mipmap-mdpi-v4/ic_launcher.png",
            "res/mipmap-mdpi/ic_launcher.png",

            "res/mipmap-xxxhdpi-v4/ic_launcher.webp",
            "res/mipmap-xxxhdpi/ic_launcher.webp",
            "res/mipmap-xxhdpi-v4/ic_launcher.webp",
            "res/mipmap-xxhdpi/ic_launcher.webp",
            "res/mipmap-xhdpi-v4/ic_launcher.webp",
            "res/mipmap-xhdpi/ic_launcher.webp",
            "res/mipmap-hdpi-v4/ic_launcher.webp",
            "res/mipmap-hdpi/ic_launcher.webp",
            "res/mipmap-mdpi-v4/ic_launcher.webp",
            "res/mipmap-mdpi/ic_launcher.webp",

            "res/drawable-xxxhdpi-v4/ic_launcher.png",
            "res/drawable-xxxhdpi/ic_launcher.png",
            "res/drawable-xxhdpi-v4/ic_launcher.png",
            "res/drawable-xxhdpi/ic_launcher.png",
            "res/drawable-xhdpi-v4/ic_launcher.png",
            "res/drawable-xhdpi/ic_launcher.png",
            "res/drawable-hdpi-v4/ic_launcher.png",
            "res/drawable-hdpi/ic_launcher.png",
            "res/drawable-mdpi-v4/ic_launcher.png",
            "res/drawable-mdpi/ic_launcher.png",
            "res/drawable/ic_launcher.png",

            "res/drawable-xxxhdpi-v4/ic_launcher.webp",
            "res/drawable-xxxhdpi/ic_launcher.webp",
            "res/drawable-xxhdpi-v4/ic_launcher.webp",
            "res/drawable-xxhdpi/ic_launcher.webp",
            "res/drawable-xhdpi-v4/ic_launcher.webp",
            "res/drawable-xhdpi/ic_launcher.webp",
            "res/drawable-hdpi-v4/ic_launcher.webp",
            "res/drawable-hdpi/ic_launcher.webp",
            "res/drawable-mdpi-v4/ic_launcher.webp",
            "res/drawable-mdpi/ic_launcher.webp",
            "res/drawable/ic_launcher.webp",

            "res/mipmap-anydpi-v26/ic_launcher.png",
            "res/mipmap-anydpi-v26/ic_launcher.webp",
        ];

        for candidate in icon_candidates {
            if let Ok(mut icon_file) = archive.by_name(candidate) {
                let mut buf = Vec::new();
                if icon_file.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                    icon_path_in_apk = Some(candidate.to_string());
                    icon_data = Some(buf);
                    break;
                }
            }
        }

        // If not found in standard paths, search all zip entries for ic_launcher or icon
        if icon_data.is_none() {
            for i in 0..archive.len() {
                if let Ok(mut file) = archive.by_index(i) {
                    let name = file.name().to_string();
                    let lower = name.to_lowercase();
                    if (lower.contains("ic_launcher") || lower.contains("icon"))
                        && (lower.ends_with(".png") || lower.ends_with(".webp"))
                    {
                        let mut buf = Vec::new();
                        if file.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                            icon_path_in_apk = Some(name);
                            icon_data = Some(buf);
                            break;
                        }
                    }
                }
            }
        }

        // App label fallback to package suffix if not resolved
        let label = app_label.unwrap_or_else(|| {
            package_name
                .split('.')
                .last()
                .unwrap_or("Android App")
                .to_string()
        });

        Ok(ApkMetadata {
            file_path: path_ref.to_string_lossy().to_string(),
            package_name,
            version_code,
            version_name,
            app_label: Some(label),
            main_activity,
            permissions,
            icon_path_in_apk,
            icon_data,
        })
    }
}
