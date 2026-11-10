#![allow(dead_code, unused_imports)]

use anyhow::{bail, Context, Result};
use libc::{unshare, CLONE_NEWIPC, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER, CLONE_NEWUTS};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ContainerConfig {
    pub rootfs_path: PathBuf,
    pub binderfs_path: PathBuf,
    pub wayland_socket: Option<String>,
    pub dri_render_node: Option<PathBuf>,
    pub pulse_socket: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub hostname: String,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));

        let pulse_path = PathBuf::from(&xdg_runtime).join("pulse/native");
        let pipewire_path = PathBuf::from(&xdg_runtime).join("pipewire-0");
        let pulse_socket = if pulse_path.exists() {
            Some(pulse_path)
        } else if pipewire_path.exists() {
            Some(pipewire_path)
        } else {
            None
        };

        Self {
            rootfs_path: PathBuf::from(&home).join(".local/share/rustydroid/rootfs"),
            binderfs_path: PathBuf::from(&home).join(".local/share/rustydroid/binderfs"),
            wayland_socket: wayland_display,
            dri_render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            pulse_socket,
            data_dir: PathBuf::from(&home).join(".local/share/rustydroid/data"),
            hostname: "rustydroid".to_string(),
        }
    }
}

pub struct NamespaceManager;

impl NamespaceManager {
    /// Checks whether user namespaces are supported by the system
    pub fn is_unshare_supported() -> bool {
        if let Ok(unprivileged) = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
            if unprivileged.trim() == "0" {
                return false;
            }
        }
        true
    }

    /// Sets up UID and GID mapping for rootless container execution
    pub fn setup_id_maps(target_uid: u32, target_gid: u32) -> Result<()> {
        let current_uid = unsafe { libc::getuid() };
        let current_gid = unsafe { libc::getgid() };

        // 1. Deny setgroups to allow writing gid_map as unprivileged user
        let setgroups_path = "/proc/self/setgroups";
        if Path::new(setgroups_path).exists() {
            if let Ok(mut f) = File::create(setgroups_path) {
                let _ = f.write_all(b"deny");
            }
        }

        // 2. Map container target_uid (usually 0) to host current_uid
        let uid_map = format!("{} {} 1\n", target_uid, current_uid);
        fs::write("/proc/self/uid_map", uid_map)
            .context("Failed to write /proc/self/uid_map")?;

        // 3. Map container target_gid (usually 0) to host current_gid
        let gid_map = format!("{} {} 1\n", target_gid, current_gid);
        fs::write("/proc/self/gid_map", gid_map)
            .context("Failed to write /proc/self/gid_map")?;

        Ok(())
    }

    /// Enter new namespaces: User, Mount, UTS, IPC
    pub fn unshare_namespaces() -> Result<()> {
        let flags = CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC;
        let res = unsafe { unshare(flags) };
        if res != 0 {
            let err = std::io::Error::last_os_error();
            bail!("unshare namespaces failed: {}", err);
        }
        Self::setup_id_maps(0, 0)?;
        Ok(())
    }

    /// Builds a bubblewrap command line for running a process inside rootless container
    pub fn build_bwrap_command(config: &ContainerConfig, cmd_args: &[&str]) -> Result<Command> {
        let mut cmd = Command::new("bwrap");

        // 1. Isolate User, PID, IPC, UTS namespaces and map container to root (UID 0 / GID 0)
        cmd.arg("--unshare-user")
           .arg("--uid").arg("0")
           .arg("--gid").arg("0")
           .arg("--unshare-pid")
           .arg("--unshare-ipc")
           .arg("--unshare-uts")
           .arg("--hostname").arg(&config.hostname);

        // Allow capability for rootless user namespace operations
        cmd.arg("--cap-add").arg("CAP_SYS_ADMIN");

        // 2. Use tmpfs root to prevent read-only root collisions and enable dynamic directory creation
        cmd.arg("--tmpfs").arg("/");

        // 3. Ensure mount point directories exist before mounting / binding
        cmd.arg("--dir").arg("/proc")
           .arg("--proc").arg("/proc")
           .arg("--dir").arg("/dev")
           .arg("--dev").arg("/dev")
           .arg("--dir").arg("/dev/binderfs")
           .arg("--dir").arg("/data");

        // 4. Bind rootfs layers or fallback to host system
        if config.rootfs_path.exists() && config.rootfs_path.join("init").exists() {
            if let Ok(entries) = fs::read_dir(&config.rootfs_path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name();
                    let target = format!("/{}", name.to_string_lossy());
                    if path.is_dir() {
                        cmd.arg("--dir").arg(&target);
                    }
                    cmd.arg("--ro-bind").arg(&path).arg(&target);
                }
            }
        } else {
            // Development and host tooling fallback
            if Path::new("/usr").exists() {
                cmd.arg("--ro-bind").arg("/usr").arg("/usr");
                cmd.arg("--symlink").arg("/usr/lib").arg("/lib");
                if Path::new("/lib64").exists() || Path::new("/usr/lib64").exists() {
                    let target = if Path::new("/usr/lib64").exists() { "/usr/lib64" } else { "/usr/lib" };
                    cmd.arg("--symlink").arg(target).arg("/lib64");
                }
                cmd.arg("--symlink").arg("/usr/bin").arg("/bin");
                cmd.arg("--symlink").arg("/usr/bin").arg("/sbin");
            }
            if Path::new("/etc").exists() {
                cmd.arg("--ro-bind-try").arg("/etc").arg("/etc");
            }
        }

        // Provide access to current executable if available
        if let Ok(current_exe) = std::env::current_exe() {
            if current_exe.exists() {
                cmd.arg("--ro-bind-try").arg(&current_exe).arg("/rustydroid");
            }
        }

        // 5. Bind GPU DRI render nodes for hardware acceleration
        if let Some(ref dri_node) = config.dri_render_node {
            if dri_node.exists() {
                cmd.arg("--dev-bind").arg(dri_node).arg(dri_node);
            }
        }
        if Path::new("/dev/dri/card0").exists() {
            cmd.arg("--dev-bind-try").arg("/dev/dri/card0").arg("/dev/dri/card0");
        }

        // 6. Bind Wayland compositor socket
        if let Some(ref wl) = config.wayland_socket {
            let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
            if !xdg_runtime.is_empty() {
                let wl_socket_path = PathBuf::from(&xdg_runtime).join(wl);
                if wl_socket_path.exists() {
                    cmd.arg("--bind").arg(&wl_socket_path).arg(&wl_socket_path);
                    cmd.env("WAYLAND_DISPLAY", wl);
                    cmd.env("XDG_RUNTIME_DIR", &xdg_runtime);
                }
            }
        }

        // 7. Bind PulseAudio / PipeWire socket
        if let Some(ref pulse) = config.pulse_socket {
            if pulse.exists() {
                cmd.arg("--bind").arg(pulse).arg(pulse);
            }
        }

        // 8. Bind binderfs devices if available
        if config.binderfs_path.exists() {
            cmd.arg("--bind").arg(&config.binderfs_path).arg("/dev/binderfs");
        }

        // 9. Bind persistent data directory
        let _ = fs::create_dir_all(&config.data_dir);
        cmd.arg("--bind").arg(&config.data_dir).arg("/data");

        // Set executable arguments
        for arg in cmd_args {
            cmd.arg(arg);
        }

        Ok(cmd)
    }
}
