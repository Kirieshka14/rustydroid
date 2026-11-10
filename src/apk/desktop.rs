#![allow(dead_code, unused_imports)]

use super::inspector::ApkMetadata;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Sanitize a string for use as a desktop entry value (e.g. Name, Comment).
/// Strips carriage returns and replaces newlines with spaces to prevent
/// arbitrary key/value or section injection into the .desktop INI format.
pub fn sanitize_desktop_entry_value(value: &str) -> String {
    value
        .chars()
        .filter(|&c| c != '\r')
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Sanitize package name to ensure it contains no newlines, carriage returns,
/// or directory separators that could corrupt desktop entries or paths.
pub fn sanitize_package_name(package_name: &str) -> String {
    let sanitized: String = package_name
        .chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '/' && c != '\\')
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "unknown.package".to_string()
    } else {
        trimmed.to_string()
    }
}

pub struct DesktopIntegration;

impl DesktopIntegration {
    /// Returns the XDG data directory respecting $XDG_DATA_HOME if set and non-empty,
    /// falling back to $HOME/.local/share.
    pub fn get_data_dir() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".local/share")
    }

    /// Returns the desktop applications directory: $XDG_DATA_HOME/applications
    /// (fallback: $HOME/.local/share/applications).
    pub fn get_applications_dir() -> PathBuf {
        Self::get_data_dir().join("applications")
    }

    /// Returns the hicolor apps icon directory: $XDG_DATA_HOME/icons/hicolor/192x192/apps
    /// (fallback: $HOME/.local/share/icons/hicolor/192x192/apps).
    pub fn get_icons_dir() -> PathBuf {
        Self::get_data_dir().join("icons/hicolor/192x192/apps")
    }

    pub fn install_desktop_entry(meta: &ApkMetadata, binary_path: Option<&str>) -> Result<PathBuf> {
        let clean_pkg = sanitize_package_name(&meta.package_name);
        let raw_label = meta.app_label.as_deref().unwrap_or(&clean_pkg);
        let sanitized_label = sanitize_desktop_entry_value(raw_label);
        let app_name = if sanitized_label.is_empty() {
            &clean_pkg
        } else {
            &sanitized_label
        };

        let apps_dir = Self::get_applications_dir();
        fs::create_dir_all(&apps_dir)
            .with_context(|| format!("Failed to create applications dir: {}", apps_dir.display()))?;

        let icon_name = format!("rustydroid-{}", clean_pkg);

        // Save icon if present
        if let Some(ref icon_bytes) = meta.icon_data {
            let icons_dir = Self::get_icons_dir();
            if fs::create_dir_all(&icons_dir).is_ok() {
                let ext = if let Some(ref p) = meta.icon_path_in_apk {
                    if p.ends_with(".webp") { "webp" } else { "png" }
                } else if icon_bytes.starts_with(b"RIFF") {
                    "webp"
                } else {
                    "png"
                };
                let icon_file = icons_dir.join(format!("{}.{}", icon_name, ext));
                let _ = fs::write(icon_file, icon_bytes);
            }
        }

        let exec_cmd = match binary_path {
            Some(bin) => {
                let clean_bin = sanitize_desktop_entry_value(bin);
                format!("{} run \"{}\"", clean_bin, clean_pkg)
            }
            None => format!("rustydroid run \"{}\"", clean_pkg),
        };

        let comment = sanitize_desktop_entry_value(&format!(
            "Android app {} running on RustyDroid",
            clean_pkg
        ));

        let desktop_content = format!(
            r#"[Desktop Entry]
Version=1.0
Type=Application
Name={name}
Comment={comment}
Exec={exec}
Icon={icon}
Terminal=false
Categories=Utility;Application;
StartupWMClass={pkg}
"#,
            name = app_name,
            comment = comment,
            pkg = clean_pkg,
            exec = exec_cmd,
            icon = icon_name,
        );

        let desktop_file = apps_dir.join(format!("rustydroid-{}.desktop", clean_pkg));
        fs::write(&desktop_file, desktop_content)
            .with_context(|| format!("Failed to write desktop file: {}", desktop_file.display()))?;

        Ok(desktop_file)
    }

    pub fn remove_desktop_entry(package_name: &str) -> Result<()> {
        let clean_pkg = sanitize_package_name(package_name);
        let apps_dir = Self::get_applications_dir();
        let desktop_file = apps_dir.join(format!("rustydroid-{}.desktop", clean_pkg));
        if desktop_file.exists() {
            let _ = fs::remove_file(desktop_file);
        }

        let icons_dir = Self::get_icons_dir();
        let icon_file_png = icons_dir.join(format!("rustydroid-{}.png", clean_pkg));
        if icon_file_png.exists() {
            let _ = fs::remove_file(icon_file_png);
        }
        let icon_file_webp = icons_dir.join(format!("rustydroid-{}.webp", clean_pkg));
        if icon_file_webp.exists() {
            let _ = fs::remove_file(icon_file_webp);
        }

        Ok(())
    }
}
