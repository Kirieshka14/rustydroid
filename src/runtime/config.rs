#![allow(dead_code, unused_imports)]

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub display_mode: DisplayMode,
    pub enable_gpu: bool,
    pub enable_audio: bool,
    pub system_image_url: String,
    pub vendor_image_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    MultiWindow,
    SingleWindow,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            display_mode: DisplayMode::MultiWindow,
            enable_gpu: true,
            enable_audio: true,
            system_image_url: "https://sourceforge.net/projects/waydroid/files/images/system/lineage/waydroid_x86_64/lineage-18.1-20240120-VANILLA-waydroid_x86_64-system.img.7z/download".to_string(),
            vendor_image_url: "https://sourceforge.net/projects/waydroid/files/images/vendor/waydroid_x86_64/lineage-18.1-20240120-MAINLINE-waydroid_x86_64-vendor.img.7z/download".to_string(),
        }
    }
}

pub struct ConfigManager;

impl ConfigManager {
    pub fn get_config_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".config/rustydroid")
    }

    pub fn get_data_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".local/share/rustydroid")
    }

    pub fn load_or_default() -> AppConfig {
        let config_file = Self::get_config_dir().join("config.json");
        if config_file.exists() {
            if let Ok(content) = fs::read_to_string(&config_file) {
                if let Ok(cfg) = serde_json::from_str(&content) {
                    return cfg;
                }
            }
        }
        let cfg = AppConfig::default();
        let _ = Self::save(&cfg);
        cfg
    }

    pub fn save(cfg: &AppConfig) -> Result<()> {
        let dir = Self::get_config_dir();
        fs::create_dir_all(&dir)?;
        let content = serde_json::to_string_pretty(cfg)?;
        fs::write(dir.join("config.json"), content)?;
        Ok(())
    }
}
