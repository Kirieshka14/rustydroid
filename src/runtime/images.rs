#![allow(dead_code, unused_imports)]

use super::config::ConfigManager;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ImageStatus {
    pub images_dir: PathBuf,
    pub rootfs_dir: PathBuf,
    pub system_img_exists: bool,
    pub vendor_img_exists: bool,
    pub rootfs_ready: bool,
}

pub struct ImageManager;

impl ImageManager {
    pub fn get_images_dir() -> PathBuf {
        ConfigManager::get_data_dir().join("images")
    }

    pub fn get_rootfs_dir() -> PathBuf {
        ConfigManager::get_data_dir().join("rootfs")
    }

    pub fn check_status() -> ImageStatus {
        let images_dir = Self::get_images_dir();
        let rootfs_dir = Self::get_rootfs_dir();

        let system_img = images_dir.join("system.img");
        let vendor_img = images_dir.join("vendor.img");
        let init_bin = rootfs_dir.join("init");

        ImageStatus {
            images_dir,
            rootfs_dir,
            system_img_exists: system_img.exists(),
            vendor_img_exists: vendor_img.exists(),
            rootfs_ready: init_bin.exists(),
        }
    }

    pub fn prepare_dirs() -> Result<()> {
        let images_dir = Self::get_images_dir();
        let rootfs_dir = Self::get_rootfs_dir();
        fs::create_dir_all(&images_dir)?;
        fs::create_dir_all(&rootfs_dir)?;
        Ok(())
    }
}
