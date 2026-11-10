#![allow(dead_code, unused_imports)]

use anyhow::{bail, Context, Result};
use libc::{c_char, c_int, c_ulong, ioctl};
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const BINDERFS_MAX_NAME: usize = 256;
pub const BINDER_CTL_ADD_IOCTL: c_ulong = 0xc1086201;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct BinderfsDevice {
    pub name: [c_char; BINDERFS_MAX_NAME],
    pub major: u32,
    pub minor: u32,
}

impl Default for BinderfsDevice {
    fn default() -> Self {
        Self {
            name: [0; BINDERFS_MAX_NAME],
            major: 0,
            minor: 0,
        }
    }
}

impl BinderfsDevice {
    pub fn new(name: &str) -> Result<Self> {
        let mut dev = Self::default();
        let bytes = name.as_bytes();
        if bytes.len() >= BINDERFS_MAX_NAME {
            bail!("Device name exceeds max binderfs name length: {}", name);
        }
        for (i, &b) in bytes.iter().enumerate() {
            dev.name[i] = b as c_char;
        }
        Ok(dev)
    }
}

pub fn add_binder_device(control_path: &Path, name: &str) -> Result<BinderfsDevice> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(control_path)
        .with_context(|| format!("Failed to open binder-control at {}", control_path.display()))?;

    let mut dev = BinderfsDevice::new(name)?;

    let res = unsafe {
        ioctl(
            file.as_raw_fd(),
            BINDER_CTL_ADD_IOCTL,
            &mut dev as *mut BinderfsDevice,
        )
    };

    if res < 0 {
        let err = std::io::Error::last_os_error();
        bail!("Failed to allocate binder device '{}' via ioctl: {}", name, err);
    }

    Ok(dev)
}
