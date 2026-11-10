#![allow(dead_code, unused_imports)]

pub mod ioctl;

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub use ioctl::{add_binder_device, BinderfsDevice};

pub struct BinderManager;

impl BinderManager {
    /// Checks whether binderfs is supported by the running Linux kernel
    pub fn is_supported() -> bool {
        // Check /proc/filesystems
        if let Ok(filesystems) = fs::read_to_string("/proc/filesystems") {
            for line in filesystems.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.contains(&"binder") || parts.contains(&"binderfs") {
                    return true;
                }
            }
        }
        // Check if /dev/binderfs already exists
        Path::new("/dev/binderfs").exists()
    }

    /// Mounts binderfs at the specified target directory with dynamic fallback between "binder" and "binderfs"
    pub fn mount_binderfs(target: &Path) -> Result<()> {
        fs::create_dir_all(target)
            .with_context(|| format!("Failed to create binderfs mount directory: {}", target.display()))?;

        let target_c = CString::new(target.to_string_lossy().as_bytes())?;

        // Determine candidate filesystem types from /proc/filesystems
        let mut candidates = Vec::new();
        if let Ok(filesystems) = fs::read_to_string("/proc/filesystems") {
            for line in filesystems.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.contains(&"binderfs") {
                    candidates.push("binderfs");
                } else if parts.contains(&"binder") {
                    candidates.push("binder");
                }
            }
        }

        // Ensure both types are probed across Linux 7.2 (Rust binder) and mainline C kernels
        if !candidates.contains(&"binder") {
            candidates.push("binder");
        }
        if !candidates.contains(&"binderfs") {
            candidates.push("binderfs");
        }

        let mut last_err = None;
        for &fstype_str in &candidates {
            let source = CString::new(fstype_str)?;
            let fstype = CString::new(fstype_str)?;

            let res = unsafe {
                libc::mount(
                    source.as_ptr(),
                    target_c.as_ptr(),
                    fstype.as_ptr(),
                    0,
                    std::ptr::null(),
                )
            };

            if res == 0 {
                return Ok(());
            } else {
                last_err = Some(std::io::Error::last_os_error());
            }
        }

        let err = last_err.unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENODEV));
        bail!("Failed to mount binderfs at {}: {}", target.display(), err);
    }

    /// Sets up standard Android binder devices: binder, hwbinder, vndbinder
    pub fn setup_standard_devices(binderfs_dir: &Path) -> Result<Vec<String>> {
        let control_path = binderfs_dir.join("binder-control");
        if !control_path.exists() {
            bail!("binder-control device does not exist at {}", control_path.display());
        }

        let standard_devices = ["binder", "hwbinder", "vndbinder"];
        let mut created = Vec::new();

        for dev_name in &standard_devices {
            let dev_path = binderfs_dir.join(dev_name);
            if !dev_path.exists() {
                match add_binder_device(&control_path, dev_name) {
                    Ok(_) => {
                        created.push(dev_name.to_string());
                    }
                    Err(e) => {
                        if !dev_path.exists() {
                            return Err(e);
                        }
                    }
                }
            } else {
                created.push(dev_name.to_string());
            }

            // Set read/write permissions for container users
            if dev_path.exists() {
                let _ = fs::set_permissions(&dev_path, Permissions::from_mode(0o666));
            }
        }

        Ok(created)
    }

    /// Creates standard symlinks inside container's /dev if needed
    pub fn create_dev_symlinks(container_dev: &Path, binderfs_dir: &Path) -> Result<()> {
        for name in &["binder", "hwbinder", "vndbinder"] {
            let src = binderfs_dir.join(name);
            let dst = container_dev.join(name);
            if src.exists() && !dst.exists() {
                #[cfg(unix)]
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
        Ok(())
    }
}
