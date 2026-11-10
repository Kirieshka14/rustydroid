#![allow(dead_code, unused_imports)]

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub struct MountManager;

impl MountManager {
    /// Ensures all required Android container directories exist
    pub fn prepare_storage_tree(base_dir: &Path) -> Result<()> {
        let dirs = [
            "rootfs",
            "images",
            "data",
            "data/app",
            "data/data",
            "cache",
            "binderfs",
            "apex",
        ];

        for sub in &dirs {
            let p = base_dir.join(sub);
            fs::create_dir_all(&p)
                .with_context(|| format!("Failed to create storage dir: {}", p.display()))?;
        }

        Ok(())
    }

    /// Verifies that GPU DRI render node exists and is accessible
    pub fn verify_gpu_access() -> Option<PathBuf> {
        let render_node = PathBuf::from("/dev/dri/renderD128");
        if render_node.exists() {
            Some(render_node)
        } else {
            None
        }
    }

    /// Locates the current active Wayland socket path
    pub fn get_wayland_socket() -> Option<PathBuf> {
        if let Ok(wl_display) = std::env::var("WAYLAND_DISPLAY") {
            if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                let p = PathBuf::from(runtime_dir).join(&wl_display);
                if p.exists() {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Locates the current active PulseAudio or PipeWire audio socket path
    pub fn get_pulse_socket() -> Option<PathBuf> {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            let pulse = PathBuf::from(&runtime_dir).join("pulse/native");
            if pulse.exists() {
                return Some(pulse);
            }
            let pipewire = PathBuf::from(&runtime_dir).join("pipewire-0");
            if pipewire.exists() {
                return Some(pipewire);
            }
        }
        None
    }
}
