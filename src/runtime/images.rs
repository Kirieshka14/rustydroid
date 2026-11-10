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

    pub fn find_system_image() -> Option<PathBuf> {
        let candidates = [
            Self::get_images_dir().join("system.img"),
            PathBuf::from("/usr/share/waydroid-extra/images/system.img"),
            PathBuf::from("/var/lib/waydroid/images/system.img"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }

    pub fn find_vendor_image() -> Option<PathBuf> {
        let candidates = [
            Self::get_images_dir().join("vendor.img"),
            PathBuf::from("/usr/share/waydroid-extra/images/vendor.img"),
            PathBuf::from("/var/lib/waydroid/images/vendor.img"),
        ];
        candidates.into_iter().find(|p| p.exists())
    }

    pub fn check_status() -> ImageStatus {
        let images_dir = Self::get_images_dir();
        let rootfs_dir = Self::get_rootfs_dir();

        let system_img = Self::find_system_image();
        let vendor_img = Self::find_vendor_image();
        let init_bin = rootfs_dir.join("init");

        ImageStatus {
            images_dir,
            rootfs_dir,
            system_img_exists: system_img.is_some(),
            vendor_img_exists: vendor_img.is_some(),
            rootfs_ready: init_bin.exists() || system_img.is_some(),
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

