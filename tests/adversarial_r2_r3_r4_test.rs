use rustydroid::binder::{
    add_binder_device,
    ioctl::{BinderfsDevice, BINDER_CTL_ADD_IOCTL},
    BinderManager,
};
use rustydroid::container::{ContainerConfig, NamespaceManager};
use rustydroid::runtime::{SessionManager, SessionState, SessionStatus};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;

mod common;
use common::TestSandbox;

static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_test_env<F, R>(sandbox: &TestSandbox, f: F) -> R
where
    F: FnOnce() -> R,
{
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let orig_home = std::env::var("HOME").ok();
    let orig_xdg = std::env::var("XDG_DATA_HOME").ok();

    std::env::set_var("HOME", sandbox.home_dir());
    std::env::set_var("XDG_DATA_HOME", sandbox.data_home());

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    match orig_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match orig_xdg {
        Some(v) => std::env::set_var("XDG_DATA_HOME", v),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }

    match res {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e),
    }
}

/// 1. Binderfs device allocation ioctl BINDER_CTL_ADD (0xc1086201):
///    - invalid name lengths (> 255 chars)
///    - null bytes
///    - non-existent control nodes
///    - empty names
#[test]
fn test_adv_binder_ctl_add_name_lengths_and_nulls() {
    assert_eq!(BINDER_CTL_ADD_IOCTL, 0xc1086201);

    // Name exceeding max length (256 bytes)
    let name_256 = "a".repeat(256);
    let res = BinderfsDevice::new(&name_256);
    assert!(res.is_err(), "Name of length 256 must be rejected");
    assert!(res.err().unwrap().to_string().contains("exceeds max binderfs name length"));

    // Name exceeding max length by large margin (10,000 bytes)
    let name_large = "x".repeat(10000);
    assert!(BinderfsDevice::new(&name_large).is_err());

    // Valid max-length name (255 bytes)
    let name_255 = "b".repeat(255);
    let dev_255 = BinderfsDevice::new(&name_255).expect("Name of length 255 must succeed");
    assert_eq!(dev_255.name[255], 0, "Last byte must be null terminator");
    assert_eq!(dev_255.name[0], b'b' as libc::c_char);

    // Empty name: creates device with null at index 0 without panic
    let empty_dev = BinderfsDevice::new("").expect("Empty name should not panic");
    assert_eq!(empty_dev.name[0], 0);

    // Name with embedded null bytes: must not panic or cause memory corruption
    let null_name = "binder\0evil_suffix";
    let null_dev = BinderfsDevice::new(null_name).expect("Null bytes should not panic");
    assert_eq!(null_dev.name[6], 0);

    // Non-existent control node
    let non_existent = Path::new("/dev/non_existent_binder_control_node_xyz");
    let err = add_binder_device(non_existent, "binder");
    assert!(err.is_err(), "Non-existent control node must return Err, not panic");
    assert!(err.err().unwrap().to_string().contains("Failed to open binder-control"));

    // Regular file as control node (ioctl ENOTTY error handled cleanly)
    let tmp = tempdir().unwrap();
    let regular_file = tmp.path().join("fake_control");
    fs::write(&regular_file, b"not a control node").unwrap();
    let ioctl_err = add_binder_device(&regular_file, "binder");
    assert!(ioctl_err.is_err(), "Ioctl on regular file must fail cleanly");
}

/// 2. Binderfs device manager standard devices setup error handling
#[test]
fn test_adv_binderfs_setup_standard_devices_edge_cases() {
    let tmp = tempdir().unwrap();

    // 1. Missing binder-control
    let err = BinderManager::setup_standard_devices(tmp.path());
    assert!(err.is_err());
    assert!(err.err().unwrap().to_string().contains("binder-control device does not exist"));

    // 2. Pre-existing device nodes are reused without error
    let dummy_control = tmp.path().join("binder-control");
    fs::write(&dummy_control, b"").unwrap();
    fs::write(tmp.path().join("binder"), b"").unwrap();
    fs::write(tmp.path().join("hwbinder"), b"").unwrap();
    fs::write(tmp.path().join("vndbinder"), b"").unwrap();

    let created = BinderManager::setup_standard_devices(tmp.path()).expect("Should succeed when all devices exist");
    assert_eq!(created.len(), 3);
    assert_eq!(created, vec!["binder", "hwbinder", "vndbinder"]);
}

/// 3. Unprivileged namespace creation and UID/GID mapping with /proc/self/setgroups = "deny"
#[test]
fn test_adv_unprivileged_namespaces_and_id_maps() {
    if !NamespaceManager::is_unshare_supported() {
        eprintln!("Unprivileged userns not supported, skipping");
        return;
    }

    // A. Verify that calling setup_id_maps outside userns fails gracefully without panic
    let current_uid = unsafe { libc::getuid() };
    if current_uid != 0 {
        let err = NamespaceManager::setup_id_maps(0, 0);
        // Writing to /proc/self/uid_map without user namespace returns EPERM
        assert!(err.is_err(), "setup_id_maps outside user namespace must return Err, not panic");
    }

    // B. Verify unshare + setgroups=deny + uid/gid mapping in a dedicated subprocess
    let script = r#"
        import os, sys, ctypes

        libc = ctypes.CDLL(None)
        CLONE_NEWUSER = 0x10000000
        CLONE_NEWNS   = 0x00020000
        CLONE_NEWUTS  = 0x04000000
        CLONE_NEWIPC  = 0x08000000

        host_uid = os.getuid()
        host_gid = os.getgid()

        # Enter unprivileged user namespace
        res = libc.unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC)
        if res != 0:
            sys.exit(1)

        # 1. Write deny to setgroups
        if os.path.exists("/proc/self/setgroups"):
            with open("/proc/self/setgroups", "w") as f:
                f.write("deny")

        # 2. Map container UID 0 to host UID
        with open("/proc/self/uid_map", "w") as f:
            f.write(f"0 {host_uid} 1\n")

        # 3. Map container GID 0 to host GID
        with open("/proc/self/gid_map", "w") as f:
            f.write(f"0 {host_gid} 1\n")

        # Verify inside namespace we are root (UID 0, GID 0)
        assert os.getuid() == 0, f"Expected UID 0 inside namespace, got {os.getuid()}"
        assert os.getgid() == 0, f"Expected GID 0 inside namespace, got {os.getgid()}"

        # Verify setgroups is deny
        with open("/proc/self/setgroups") as f:
            content = f.read().strip()
            assert content == "deny", f"Expected setgroups deny, got {content}"

        print("USERNS_SUCCESS")
    "#;

    let output = Command::new("python3")
        .args(&["-c", script])
        .output()
        .expect("Failed to execute userns verification subprocess");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("USERNS_SUCCESS"),
        "Unprivileged userns creation and UID/GID mapping failed: stdout='{}', stderr='{}'",
        stdout,
        stderr
    );
}

/// 4. Bubblewrap command construction: verify mount directories are never created on read-only parents
#[test]
fn test_adv_bwrap_mount_hierarchy_no_readonly_parents() {
    let mut config = ContainerConfig::default();
    let tmp = tempdir().unwrap();

    let rootfs_dir = tmp.path().join("rootfs");
    fs::create_dir_all(&rootfs_dir).unwrap();
    fs::write(rootfs_dir.join("init"), b"#!/bin/sh\nexit 0").unwrap();
    fs::create_dir_all(rootfs_dir.join("system")).unwrap();
    fs::create_dir_all(rootfs_dir.join("vendor")).unwrap();

    config.rootfs_path = rootfs_dir;
    config.data_dir = tmp.path().join("data");
    config.binderfs_path = tmp.path().join("binderfs");

    let cmd = NamespaceManager::build_bwrap_command(&config, &["sh", "-c", "echo bwrap_ok"]).unwrap();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();

    // 1. Root MUST be tmpfs so root is writable in-memory and permits creating directories
    assert!(args.contains(&"--tmpfs".to_string()));
    let tmpfs_idx = args.iter().position(|r| r == "--tmpfs").unwrap();
    assert_eq!(args[tmpfs_idx + 1], "/");

    // 2. Ensure all mount directories (--dir) are declared before their corresponding mounts
    let mut readonly_paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--ro-bind" || args[i] == "--ro-bind-try" {
            let dest = &args[i + 2];
            readonly_paths.push(dest.clone());
            i += 3;
        } else if args[i] == "--dir" {
            let dir_path = &args[i + 1];
            // Verify dir_path is NOT a subpath of an already declared read-only bind mount
            for ro in &readonly_paths {
                assert!(
                    !dir_path.starts_with(&format!("{}/", ro)),
                    "Mount directory '{}' cannot be created on read-only parent '{}'",
                    dir_path,
                    ro
                );
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    // 3. Empirically verify that writing inside the container's tmpfs/data paths succeeds
    if Path::new("/usr/bin/bwrap").exists() {
        let test_cfg = ContainerConfig::default();
        let mut test_cmd = NamespaceManager::build_bwrap_command(
            &test_cfg,
            &["sh", "-c", "mkdir -p /data/adv_test && touch /data/adv_test/emp_check && ls /data/adv_test/emp_check"],
        ).unwrap();

        let out = test_cmd.output().expect("Failed to execute bwrap");
        assert!(out.status.success(), "Directory creation inside container data path must succeed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("emp_check"));
    }
}

/// 5. Session lifecycle resilience:
///    - Starting a session when another is already running
///    - Stopping a session when no session is active
#[test]
fn test_adv_session_lifecycle_concurrent_and_no_session() {
    let sandbox = TestSandbox::new().unwrap();

    with_test_env(&sandbox, || {
        // A. Stopping a session when no session is active: must return Ok, no panic
        let stop_res = SessionManager::stop_session();
        assert!(stop_res.is_ok(), "Stopping with no session must succeed");
        assert_eq!(SessionManager::get_status(), SessionStatus::Stopped);

        // B. Start a mock session
        let mock_proc = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("Failed to spawn mock supervisor");
        let mock_pid = mock_proc.id();

        let state = SessionState {
            pid: mock_pid,
            started_at: 1000,
            display_mode: "multi_window".to_string(),
            is_active: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        fs::write(SessionManager::get_session_file(), json).unwrap();

        // Verify session is active
        assert!(SessionManager::is_running());
        assert_eq!(SessionManager::get_state().unwrap().pid, mock_pid);

        // Starting a session when already active returns the existing PID without creating a new process
        let started_pid = SessionManager::start_session().unwrap();
        assert_eq!(started_pid, mock_pid);

        // Clean up
        SessionManager::stop_session().unwrap();
        assert!(!SessionManager::check_pid_alive(mock_pid));
        assert_eq!(SessionManager::get_status(), SessionStatus::Stopped);
    });
}

/// 6. Session lifecycle resilience:
///    - Handling dead/stale session.json gracefully
///    - Handling corrupted / truncated / zero-byte session.json
#[test]
fn test_adv_session_stale_and_corrupted_json_handling() {
    let sandbox = TestSandbox::new().unwrap();

    with_test_env(&sandbox, || {
        let session_file = SessionManager::get_session_file();

        // Case 1: Dead PID (e.g. 999999)
        let stale_state = SessionState {
            pid: 999999,
            started_at: 1000,
            display_mode: "multi_window".to_string(),
            is_active: true,
        };
        fs::write(&session_file, serde_json::to_string(&stale_state).unwrap()).unwrap();
        assert!(session_file.exists());

        // Calling get_state should detect the dead PID, purge the file, and return None
        let st = SessionManager::get_state();
        assert!(st.is_none(), "Dead PID must return None");
        assert!(!session_file.exists(), "Stale session.json must be removed automatically");
        assert_eq!(SessionManager::get_status(), SessionStatus::Stopped);

        // Case 2: PID 0
        let zero_state = SessionState {
            pid: 0,
            started_at: 1000,
            display_mode: "multi_window".to_string(),
            is_active: true,
        };
        fs::write(&session_file, serde_json::to_string(&zero_state).unwrap()).unwrap();
        let st_zero = SessionManager::get_state();
        assert!(st_zero.is_none());
        assert!(!session_file.exists(), "PID 0 session.json must be purged");

        // Case 3: Corrupted JSON content
        fs::write(&session_file, b"{{NOT_VALID_JSON: true").unwrap();
        assert!(SessionManager::get_state().is_none(), "Corrupted JSON must return None without panic");
        assert_eq!(SessionManager::get_status(), SessionStatus::Stopped);

        // stop_session should remove the corrupted file cleanly
        SessionManager::stop_session().unwrap();
        assert!(!session_file.exists());

        // Case 4: Zero-byte file
        fs::write(&session_file, b"").unwrap();
        assert!(SessionManager::get_state().is_none(), "Zero-byte file must return None without panic");
        SessionManager::stop_session().unwrap();
        assert!(!session_file.exists());
    });
}

/// 7. Two-stage shutdown protocol: SIGTERM followed by SIGKILL fallback
#[test]
fn test_adv_two_stage_shutdown_sigterm_then_sigkill() {
    let sandbox = TestSandbox::new().unwrap();

    with_test_env(&sandbox, || {
        // Spawn a process that explicitly ignores SIGTERM ('trap "" TERM')
        let child = Command::new("sh")
            .args(&["-c", "trap '' TERM; while true; do sleep 1; done"])
            .spawn()
            .expect("Failed to spawn process ignoring SIGTERM");

        let pid = child.id();
        std::thread::sleep(Duration::from_millis(100));
        assert!(SessionManager::check_pid_alive(pid), "Process must be alive");

        let state = SessionState {
            pid,
            started_at: 1000,
            display_mode: "multi_window".to_string(),
            is_active: true,
        };
        fs::write(SessionManager::get_session_file(), serde_json::to_string(&state).unwrap()).unwrap();

        // Call stop_session: SIGTERM is ignored, but SIGKILL fallback will terminate it
        let start_time = std::time::Instant::now();
        SessionManager::stop_session().expect("stop_session must succeed");
        let elapsed = start_time.elapsed();

        // Verify elapsed time is around 2 seconds (the SIGTERM wait loop)
        assert!(
            elapsed >= Duration::from_millis(1800),
            "Two-stage shutdown should wait up to 2 seconds for SIGTERM before SIGKILL fallback (elapsed: {:?})",
            elapsed
        );

        // Verify process was killed by SIGKILL
        assert!(!SessionManager::check_pid_alive(pid), "Process must be dead after SIGKILL fallback");
        assert!(!SessionManager::get_session_file().exists(), "session.json must be removed");
    });
}

/// 8. Child subreaping: ensuring orphaned child processes don't become zombies
#[test]
fn test_adv_child_subreaping_and_zombie_prevention() {
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 1. Enable child subreaper in current process
    unsafe {
        let ret = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        assert_eq!(ret, 0, "prctl PR_SET_CHILD_SUBREAPER must succeed");

        let mut is_subreaper: libc::c_int = 0;
        libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut is_subreaper, 0, 0, 0);
        assert_eq!(is_subreaper, 1);
    }

    // 2. Spawn a child that spawns a grandchild and exits immediately
    // The grandchild sleeps briefly, exiting as an orphan
    let script = r#"
        import os, sys, time
        # Fork grandchild
        pid = os.fork()
        if pid == 0:
            # Grandchild: sleep briefly then exit
            time.sleep(0.2)
            sys.exit(42)
        else:
            # Child prints grandchild pid and exits immediately, orphaning grandchild
            print(pid)
            sys.stdout.flush()
            sys.exit(0)
    "#;

    let child = Command::new("python3")
        .args(&["-c", script])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to spawn parent");

    let output = child.wait_with_output().expect("Failed to wait for child");
    assert!(output.status.success());
    let grandchild_pid: i32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("Failed to parse grandchild pid");

    // Give grandchild time to reparent and exit
    std::thread::sleep(Duration::from_millis(300));

    // Reap reparented orphan process to prevent zombies
    unsafe {
        let mut status: libc::c_int = 0;
        let reaped_pid = libc::waitpid(grandchild_pid, &mut status, libc::WNOHANG);
        assert_eq!(reaped_pid, grandchild_pid, "Must reap specific reparented grandchild");
        let exited = libc::WIFEXITED(status);
        let code = libc::WEXITSTATUS(status);
        if exited {
            assert_eq!(code, 42, "Orphan child was reaped with expected exit code 42");
        }
        // Restore subreaper state to 0 so other tests are not affected
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0);
    }
}

/// 9. CLI commands: verify `rustydroid status`, `rustydroid stop`, `rustydroid init`, `rustydroid run`
///    error gracefully on missing or invalid inputs without panics.
#[test]
fn test_adv_cli_graceful_error_handling() {
    let sandbox = TestSandbox::new().unwrap();

    // A. `rustydroid status`
    let (status, stdout, stderr) = sandbox.run_cli(&["status"]).unwrap();
    assert!(status.success(), "status should exit 0");
    assert!(stdout.contains("RustyDroid System Diagnosis"));
    assert!(!stderr.contains("panicked at"));

    // B. `rustydroid stop` when no session active
    let (status, stdout, stderr) = sandbox.run_cli(&["stop"]).unwrap();
    assert!(status.success(), "stop should exit 0 when no session");
    assert!(stdout.contains("No active session found"));
    assert!(!stderr.contains("panicked at"));

    // C. `rustydroid init`
    let (status, stdout, stderr) = sandbox.run_cli(&["init"]).unwrap();
    assert!(status.success(), "init should exit 0");
    assert!(stdout.contains("Storage tree prepared"));
    assert!(!stderr.contains("panicked at"));

    // D. `rustydroid run` missing argument
    let (status, _stdout, stderr) = sandbox.run_cli(&["run"]).unwrap();
    assert_eq!(status.code(), Some(2), "Missing argument should exit with code 2 (clap)");
    assert!(stderr.contains("the following required arguments were not provided"));
    assert!(!stderr.contains("panicked at"));

    // E. `rustydroid run nonexistent.apk`
    let (status, stdout, stderr) = sandbox.run_cli(&["run", "nonexistent_file_12345.apk"]).unwrap();
    assert_eq!(status.code(), Some(1), "Missing file should exit with code 1");
    assert!(stderr.contains("Failed to open APK file") || stdout.contains("Failed to open APK file"));
    assert!(!stderr.contains("panicked at"));

    // F. `rustydroid info nonexistent.apk`
    let (status, _stdout, stderr) = sandbox.run_cli(&["info", "nonexistent_file_12345.apk"]).unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("Failed to open APK file"));
    assert!(!stderr.contains("panicked at"));

    // G. `rustydroid install nonexistent.apk`
    let (status, _stdout, stderr) = sandbox.run_cli(&["install", "nonexistent_file_12345.apk"]).unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("Failed to open APK file"));
    assert!(!stderr.contains("panicked at"));

    // H. `rustydroid status` with corrupted session.json
    fs::write(sandbox.data_dir().join("session.json"), b"{{CORRUPTED_JSON").unwrap();
    let (status, stdout, stderr) = sandbox.run_cli(&["status"]).unwrap();
    assert!(status.success(), "status with corrupted session.json must succeed cleanly");
    assert!(stdout.contains("STOPPED"));
    assert!(!stderr.contains("panicked at"));
}
