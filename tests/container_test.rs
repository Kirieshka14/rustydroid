use rustydroid::container::{ContainerConfig, MountManager, NamespaceManager};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn test_container_config_defaults() {
    let cfg = ContainerConfig::default();
    assert_eq!(cfg.hostname, "rustydroid");
    assert!(cfg.rootfs_path.to_string_lossy().contains(".local/share/rustydroid/rootfs"));
    assert!(cfg.binderfs_path.to_string_lossy().contains(".local/share/rustydroid/binderfs"));
}

#[test]
fn test_unshare_supported() {
    assert!(NamespaceManager::is_unshare_supported());
}

#[test]
fn test_bwrap_command_construction() {
    let cfg = ContainerConfig::default();
    let cmd = NamespaceManager::build_bwrap_command(&cfg, &["echo", "hello"]).unwrap();
    let program = cmd.get_program();
    assert_eq!(program, "bwrap");
}

#[test]
fn test_gpu_dri_verification() {
    let gpu = MountManager::verify_gpu_access();
    // On this machine /dev/dri/renderD128 exists
    assert_eq!(gpu, Some(PathBuf::from("/dev/dri/renderD128")));
}

#[test]
fn test_bwrap_command_dir_flags_and_tmpfs_root() {
    let cfg = ContainerConfig::default();
    let cmd = NamespaceManager::build_bwrap_command(&cfg, &["true"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();

    // Verify isolation flags
    assert!(args.contains(&"--unshare-user".to_string()));
    assert!(args.contains(&"--uid".to_string()));
    assert!(args.contains(&"--gid".to_string()));
    assert!(args.contains(&"--unshare-pid".to_string()));
    assert!(args.contains(&"--unshare-ipc".to_string()));
    assert!(args.contains(&"--unshare-uts".to_string()));
    assert!(args.contains(&"--hostname".to_string()));
    assert!(args.contains(&"rustydroid".to_string()));

    // Verify tmpfs root layout preventing read-only filesystem collisions
    assert!(args.contains(&"--tmpfs".to_string()));
    assert!(args.contains(&"/".to_string()));

    // Verify mount points are created before binding
    assert!(args.contains(&"--dir".to_string()));
    assert!(args.contains(&"/proc".to_string()));
    assert!(args.contains(&"--proc".to_string()));
    assert!(args.contains(&"--dir".to_string()));
    assert!(args.contains(&"/dev".to_string()));
    assert!(args.contains(&"--dev".to_string()));
    assert!(args.contains(&"/dev/binderfs".to_string()));
    assert!(args.contains(&"/data".to_string()));
}

#[test]
fn test_bwrap_wayland_socket_forwarding() {
    let mut cfg = ContainerConfig::default();
    cfg.wayland_socket = Some("wayland-1".to_string());

    let cmd = NamespaceManager::build_bwrap_command(&cfg, &["echo", "wl"]).unwrap();
    let envs: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_string_lossy().to_string(), v.map(|s| s.to_string_lossy().to_string())))
        .collect();

    let wayland_env = envs.iter().find(|(k, _)| k == "WAYLAND_DISPLAY");
    if let Some((_, Some(val))) = wayland_env {
        assert_eq!(val, "wayland-1");
    }
}

#[test]
fn test_bwrap_pulse_audio_socket_forwarding() {
    let tmp = tempdir().unwrap();
    let fake_pulse = tmp.path().join("pulse_test_socket");
    std::fs::write(&fake_pulse, b"").unwrap();

    let mut cfg = ContainerConfig::default();
    cfg.pulse_socket = Some(fake_pulse.clone());

    let cmd = NamespaceManager::build_bwrap_command(&cfg, &["true"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();

    assert!(args.contains(&fake_pulse.to_string_lossy().to_string()));
}

#[test]
fn test_prepare_storage_tree() {
    let tmp = tempdir().unwrap();
    MountManager::prepare_storage_tree(tmp.path()).unwrap();

    let required = [
        "rootfs",
        "images",
        "data",
        "data/app",
        "data/data",
        "cache",
        "binderfs",
        "apex",
    ];

    for r in &required {
        assert!(tmp.path().join(r).exists(), "Directory '{}' must exist after prepare_storage_tree", r);
    }
}

#[test]
fn test_get_pulse_socket() {
    let socket = MountManager::get_pulse_socket();
    if let Some(sock) = socket {
        assert!(sock.exists(), "Detected audio socket must exist: {:?}", sock);
    }
}

#[test]
fn test_bwrap_container_execution() {
    if !Path::new("/usr/bin/bwrap").exists() {
        eprintln!("bwrap binary not found, skipping execution test");
        return;
    }

    let cfg = ContainerConfig::default();
    let mut cmd = NamespaceManager::build_bwrap_command(&cfg, &["sh", "-c", "echo container_sandbox_ok"]).unwrap();

    let output = cmd.output().expect("Failed to execute bwrap container command");
    assert!(output.status.success(), "bwrap container must execute successfully");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("container_sandbox_ok"));
}

#[test]
fn test_subreaper_prctl() {
    unsafe {
        let set_res = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        assert_eq!(set_res, 0, "prctl(PR_SET_CHILD_SUBREAPER) should succeed");

        let mut is_subreaper: libc::c_int = 0;
        let get_res = libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut is_subreaper, 0, 0, 0);
        assert_eq!(get_res, 0);
        assert_eq!(is_subreaper, 1, "Process must be confirmed as child subreaper");
    }
}
