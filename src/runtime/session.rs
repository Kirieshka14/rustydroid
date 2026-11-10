#![allow(dead_code, unused_imports)]

use super::config::ConfigManager;
use crate::binder::BinderManager;
use crate::container::{ContainerConfig, MountManager, NamespaceManager};
use anyhow::{Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub pid: u32,
    pub started_at: u64,
    pub display_mode: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Running(SessionState),
    Stopped,
}

pub struct SessionManager;

impl SessionManager {
    pub fn get_session_file() -> PathBuf {
        ConfigManager::get_data_dir().join("session.json")
    }

    pub fn get_state() -> Option<SessionState> {
        let path = Self::get_session_file();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(state) = serde_json::from_str::<SessionState>(&content) {
                    if Self::check_pid_alive(state.pid) {
                        return Some(state);
                    } else {
                        // Clean up stale session.json immediately when supervisor process is dead
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
        None
    }

    pub fn get_status() -> SessionStatus {
        match Self::get_state() {
            Some(state) => SessionStatus::Running(state),
            None => SessionStatus::Stopped,
        }
    }

    pub fn is_running() -> bool {
        Self::get_state().is_some()
    }

    pub fn check_pid_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }

        // 1. Non-blocking reap attempt if this process is a direct or reparented child that exited
        unsafe {
            let mut st: libc::c_int = 0;
            let res = libc::waitpid(pid as i32, &mut st, libc::WNOHANG);
            if res > 0 {
                return false;
            }
        }

        // 2. Check existence via signal 0
        let nix_pid = Pid::from_raw(pid as i32);
        if kill(nix_pid, None).is_err() {
            return false;
        }

        // 3. Inspect /proc/<pid>/status to filter out zombie / defunct / dead states
        let status_path = format!("/proc/{}/status", pid);
        if let Ok(content) = fs::read_to_string(&status_path) {
            for line in content.lines() {
                if line.starts_with("State:") {
                    if line.contains('Z') || line.contains("zombie") || line.contains('X') || line.contains("dead") {
                        // Attempt to reap child if it was our direct/reparented child
                        unsafe {
                            let mut st: libc::c_int = 0;
                            let _ = libc::waitpid(pid as i32, &mut st, libc::WNOHANG);
                        }
                        return false;
                    }
                    break;
                }
            }
        } else {
            return false;
        }

        true
    }

    pub fn start_session() -> Result<u32> {
        if let Some(state) = Self::get_state() {
            return Ok(state.pid);
        }

        // Activate PR_SET_CHILD_SUBREAPER so orphaned container processes are reparented
        // to this process rather than host systemd/init, avoiding zombie leaks.
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        }

        let data_dir = ConfigManager::get_data_dir();
        MountManager::prepare_storage_tree(&data_dir)?;

        let config = ContainerConfig::default();

        // Supervisor script mounts binderfs and configures standard devices inside the unprivileged
        // container user namespace where CAP_SYS_ADMIN is available, avoiding host-level EPERM.
        let supervisor_script = r#"
if [ -d /dev/binderfs ]; then
    if [ -x /rustydroid ]; then
        /rustydroid internal-binderfs /dev/binderfs 2>/dev/null || true
    fi
    if [ ! -e /dev/binderfs/binder ]; then
        mount -t binder binder /dev/binderfs 2>/dev/null || mount -t binderfs binderfs /dev/binderfs 2>/dev/null || true
        python3 -c '
import fcntl, struct, os
try:
    fd = os.open("/dev/binderfs/binder-control", os.O_RDWR)
    for dev in ["binder", "hwbinder", "vndbinder"]:
        buf = bytearray(dev.encode("utf-8").ljust(256, b"\0") + struct.pack("II", 0, 0))
        try: fcntl.ioctl(fd, 0xc1086201, buf)
        except Exception: pass
        try: os.chmod(f"/dev/binderfs/{dev}", 0o666)
        except Exception: pass
    os.close(fd)
except Exception: pass
' 2>/dev/null || true
    fi
fi
while true; do
    sleep 3600 &
    wait $!
done
"#;

        // Launch background container supervisor in its own detached process group
        let mut bwrap_cmd = NamespaceManager::build_bwrap_command(&config, &["sh", "-c", supervisor_script])?;
        bwrap_cmd.stdout(Stdio::null());
        bwrap_cmd.stderr(Stdio::null());

        use std::os::unix::process::CommandExt;
        bwrap_cmd.process_group(0);

        let child = bwrap_cmd.spawn().context("Failed to spawn container supervisor with bwrap")?;
        let pid = child.id();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let state = SessionState {
            pid,
            started_at: now,
            display_mode: "multi_window".to_string(),
            is_active: true,
        };

        let json = serde_json::to_string_pretty(&state)?;
        fs::write(Self::get_session_file(), json)?;

        Ok(pid)
    }

    pub fn stop_session() -> Result<()> {
        let path = Self::get_session_file();
        if let Some(state) = Self::get_state() {
            let nix_pid = Pid::from_raw(state.pid as i32);
            let _ = kill(nix_pid, Signal::SIGTERM);
            let _ = kill(Pid::from_raw(-(state.pid as i32)), Signal::SIGTERM);

            // Wait up to 2 seconds for graceful shutdown
            for _ in 0..20 {
                unsafe {
                    let mut st: libc::c_int = 0;
                    let _ = libc::waitpid(state.pid as i32, &mut st, libc::WNOHANG);
                }
                std::thread::sleep(Duration::from_millis(100));
                if !Self::check_pid_alive(state.pid) {
                    break;
                }
            }

            if Self::check_pid_alive(state.pid) {
                let _ = kill(nix_pid, Signal::SIGKILL);
                let _ = kill(Pid::from_raw(-(state.pid as i32)), Signal::SIGKILL);
                std::thread::sleep(Duration::from_millis(50));
                unsafe {
                    let mut st: libc::c_int = 0;
                    let _ = libc::waitpid(state.pid as i32, &mut st, libc::WNOHANG);
                }
            }
        }

        if path.exists() {
            let _ = fs::remove_file(path);
        }

        Ok(())
    }
}
