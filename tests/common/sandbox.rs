#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Mutex;
use tempfile::TempDir;

pub struct TestSandbox {
    temp_dir: TempDir,
    tracked_pids: Mutex<Vec<u32>>,
}

impl TestSandbox {
    pub fn new() -> anyhow::Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let s = Self {
            temp_dir,
            tracked_pids: Mutex::new(Vec::new()),
        };
        std::fs::create_dir_all(s.apps_dir())?;
        std::fs::create_dir_all(s.icons_dir())?;
        std::fs::create_dir_all(s.data_dir())?;
        std::fs::create_dir_all(s.config_dir())?;
        std::fs::create_dir_all(s.runtime_dir())?;
        Ok(s)
    }

    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    pub fn home_dir(&self) -> PathBuf {
        self.temp_dir.path().to_path_buf()
    }

    pub fn data_home(&self) -> PathBuf {
        self.home_dir().join(".local/share")
    }

    pub fn apps_dir(&self) -> PathBuf {
        self.data_home().join("applications")
    }

    pub fn icons_dir(&self) -> PathBuf {
        self.data_home().join("icons/hicolor/192x192/apps")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_home().join("rustydroid")
    }

    pub fn config_dir(&self) -> PathBuf {
        self.home_dir().join(".config/rustydroid")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.home_dir().join("run")
    }

    pub fn cli_cmd(&self) -> Command {
        let bin_path = env!("CARGO_BIN_EXE_rustydroid");
        let mut cmd = Command::new(bin_path);
        cmd.current_dir(self.home_dir())
            .env("HOME", self.home_dir())
            .env("XDG_DATA_HOME", self.data_home())
            .env("XDG_CONFIG_HOME", self.home_dir().join(".config"))
            .env("XDG_RUNTIME_DIR", self.runtime_dir());
        cmd
    }

    pub fn run_cli(&self, args: &[&str]) -> anyhow::Result<(ExitStatus, String, String)> {
        let mut cmd = self.cli_cmd();
        for a in args {
            cmd.arg(a);
        }
        let output = cmd.output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((output.status, stdout, stderr))
    }

    pub fn track_pid(&self, pid: u32) {
        if let Ok(mut lock) = self.tracked_pids.lock() {
            lock.push(pid);
        }
    }
}

impl Drop for TestSandbox {
    fn drop(&mut self) {
        let session_file = self.data_dir().join("session.json");
        let maybe_pid = if session_file.exists() {
            std::fs::read_to_string(&session_file)
                .ok()
                .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
                .and_then(|val| val.get("pid").and_then(|p| p.as_i64()).map(|p| p as i32))
        } else {
            None
        };

        // 1. Attempt standard graceful shutdown via CLI stop
        if maybe_pid.is_some() || session_file.exists() {
            let _ = self.run_cli(&["stop"]);
        }

        // 2. Terminate session supervisor and process group if still alive
        if let Some(pid) = maybe_pid {
            if pid > 1 {
                unsafe {
                    if libc::kill(pid, 0) == 0 {
                        libc::kill(pid, libc::SIGTERM);
                        libc::kill(-pid, libc::SIGTERM);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    if libc::kill(pid, 0) == 0 {
                        libc::kill(pid, libc::SIGKILL);
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
            }
        }

        // 3. Terminate any explicitly tracked process IDs
        if let Ok(pids) = self.tracked_pids.lock() {
            for &pid in pids.iter() {
                let pid = pid as i32;
                if pid > 1 {
                    unsafe {
                        if libc::kill(pid, 0) == 0 {
                            libc::kill(pid, libc::SIGKILL);
                            libc::kill(-pid, libc::SIGKILL);
                        }
                    }
                }
            }
        }
    }
}
