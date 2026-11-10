#![allow(dead_code, unused_imports)]

use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};

pub struct AdbClient;

impl AdbClient {
    /// Checks if adb binary is available on the host system
    pub fn is_adb_available() -> bool {
        Command::new("adb")
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Installs an APK into the running container
    pub fn install_apk(apk_path: &str) -> Result<String> {
        let output = Command::new("adb")
            .args(&["install", "-r", apk_path])
            .output()
            .context("Failed to execute adb install")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            bail!("adb install failed:\nSTDOUT: {}\nSTDERR: {}", stdout, stderr);
        }

        Ok(stdout)
    }

    /// Launches the main activity for a package
    pub fn start_activity(package_name: &str, activity_name: &str) -> Result<String> {
        let target = if activity_name.starts_with('.') {
            format!("{}/{}", package_name, activity_name)
        } else if !activity_name.contains('/') {
            format!("{}/{}", package_name, activity_name)
        } else {
            activity_name.to_string()
        };

        let output = Command::new("adb")
            .args(&["shell", "am", "start", "-n", &target])
            .output()
            .context("Failed to execute adb shell am start")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            bail!("am start failed: {}\n{}", stdout, stderr);
        }

        Ok(stdout)
    }

    /// Stops an application
    pub fn stop_app(package_name: &str) -> Result<()> {
        let _ = Command::new("adb")
            .args(&["shell", "am", "force-stop", package_name])
            .status();
        Ok(())
    }

    /// Lists installed third-party packages
    pub fn list_packages() -> Result<Vec<String>> {
        let output = Command::new("adb")
            .args(&["shell", "pm", "list", "packages", "-3"])
            .output()
            .context("Failed to execute pm list packages")?;

        let text = String::from_utf8_lossy(&output.stdout);
        let mut packages = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(pkg) = line.strip_prefix("package:") {
                packages.push(pkg.to_string());
            }
        }
        Ok(packages)
    }

    /// Executes arbitrary shell command inside container
    pub fn shell_command(cmd_args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("adb");
        cmd.arg("shell");
        for a in cmd_args {
            cmd.arg(a);
        }
        let output = cmd.output().context("Failed to execute adb shell command")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
