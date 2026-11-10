use rustydroid::binder::{
    ioctl::{BinderfsDevice, BINDERFS_MAX_NAME, BINDER_CTL_ADD_IOCTL},
    BinderManager,
};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_binderfs_support_detection() {
    let supported = BinderManager::is_supported();
    println!("Binderfs supported in kernel: {}", supported);
    // On our Linux 7.2 Arch system, binder is in /proc/filesystems
    assert!(supported);
}

#[test]
fn test_binder_ioctl_constant() {
    assert_eq!(BINDER_CTL_ADD_IOCTL, 0xc1086201);
}

#[test]
fn test_binderfs_device_struct() {
    // 1. Verify default values
    let dev = BinderfsDevice::default();
    assert_eq!(dev.major, 0);
    assert_eq!(dev.minor, 0);

    // 2. Verify new device construction
    let binder = BinderfsDevice::new("binder").expect("Failed to create binder device");
    assert_eq!(binder.major, 0);
    assert_eq!(binder.minor, 0);
    assert_eq!(binder.name[0], b'b' as libc::c_char);
    assert_eq!(binder.name[1], b'i' as libc::c_char);
    assert_eq!(binder.name[2], b'n' as libc::c_char);
    assert_eq!(binder.name[3], b'd' as libc::c_char);
    assert_eq!(binder.name[4], b'e' as libc::c_char);
    assert_eq!(binder.name[5], b'r' as libc::c_char);
    assert_eq!(binder.name[6], 0);

    // 3. Verify max name length rejection
    let long_name = "a".repeat(BINDERFS_MAX_NAME + 10);
    let err = BinderfsDevice::new(&long_name);
    assert!(err.is_err(), "Device name exceeding BINDERFS_MAX_NAME must fail");
}

#[test]
fn test_binderfs_control_missing() {
    let tmp = tempdir().unwrap();
    let res = BinderManager::setup_standard_devices(tmp.path());
    assert!(res.is_err());
    let err_msg = res.err().unwrap().to_string();
    assert!(
        err_msg.contains("binder-control device does not exist"),
        "Expected missing binder-control error, got: {}",
        err_msg
    );
}

#[test]
fn test_binder_dev_symlinks() {
    let tmp = tempdir().unwrap();
    let binderfs_dir = tmp.path().join("binderfs");
    let container_dev = tmp.path().join("dev");

    fs::create_dir_all(&binderfs_dir).unwrap();
    fs::create_dir_all(&container_dev).unwrap();

    // Create dummy device files
    fs::write(binderfs_dir.join("binder"), b"").unwrap();
    fs::write(binderfs_dir.join("hwbinder"), b"").unwrap();
    fs::write(binderfs_dir.join("vndbinder"), b"").unwrap();

    BinderManager::create_dev_symlinks(&container_dev, &binderfs_dir).unwrap();

    assert!(container_dev.join("binder").is_symlink());
    assert!(container_dev.join("hwbinder").is_symlink());
    assert!(container_dev.join("vndbinder").is_symlink());

    assert_eq!(
        fs::read_link(container_dev.join("binder")).unwrap(),
        binderfs_dir.join("binder")
    );
}

#[test]
fn test_rootless_binderfs_unshare_and_mount() {
    // Test mounting binderfs dynamically inside an unprivileged user namespace using `unshare -U -m -r`
    if !Path::new("/usr/bin/unshare").exists() {
        eprintln!("unshare not found, skipping unshare test");
        return;
    }

    let script = r#"
tmp=$(mktemp -d /tmp/rustydroid_bf_test_XXXXXX)
mount -t binder binder "$tmp" 2>/dev/null || mount -t binderfs binderfs "$tmp" 2>/dev/null
if [ -e "$tmp/binder-control" ]; then
    echo "SUCCESS"
    umount "$tmp"
fi
rm -rf "$tmp"
"#;

    let output = Command::new("unshare")
        .args(&["-U", "-m", "-r", "sh", "-c", script])
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("SUCCESS"),
                "Rootless binderfs mount should create binder-control inside userns"
            );
        }
    }
}
