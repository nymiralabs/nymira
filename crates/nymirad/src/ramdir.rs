//! A directory that lives in memory, for the wallet's working files.
//!
//! The embedded wallet reads and writes its state as plain files and cannot be pointed at a
//! buffer, so while a lease is open those files must sit somewhere that is not a disk. On Linux
//! that is a tmpfs directory under `/dev/shm`; on macOS a RAM disk made with `hdiutil`,
//! `newfs_hfs` and `mount` (all work for an ordinary user), hidden from Finder and Spotlight. If neither can be made, or the directory turns out
//! not to be memory-backed, opening fails and nothing falls back to the disk.
//!
//! The directory survives a crash of this process, and the next start finds the files again and
//! seals them (`WalletVault::seal_leftovers`). Limit: the operating system may write memory to
//! swap; macOS encrypts swap by default, Linux does not unless the machine is set up for it.

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
#[error("no memory-backed directory for the wallet files: {0}")]
pub struct RamDirError(pub String);

#[cfg(target_os = "linux")]
fn backed_by_memory(path: &std::path::Path) -> Result<bool, RamDirError> {
    use std::os::unix::ffi::OsStrExt;
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    const RAMFS_MAGIC: i64 = 0x8584_58f6;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| RamDirError(e.to_string()))?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return Err(RamDirError(std::io::Error::last_os_error().to_string()));
    }
    Ok(matches!(s.f_type as i64, TMPFS_MAGIC | RAMFS_MAGIC))
}

#[cfg(target_os = "linux")]
fn make() -> Result<PathBuf, RamDirError> {
    use std::os::unix::fs::PermissionsExt;
    let dir = PathBuf::from(format!("/dev/shm/nymira-{}", unsafe { libc::geteuid() }));
    std::fs::create_dir_all(&dir).map_err(|e| RamDirError(e.to_string()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| RamDirError(e.to_string()))?;
    if !backed_by_memory(&dir)? {
        return Err(RamDirError(format!("{} is not tmpfs", dir.display())));
    }
    Ok(dir)
}

/// The device a mount point is mounted from, from the output of `mount`.
#[cfg(target_os = "macos")]
fn mounted_device(mount_output: &str, mount_point: &std::path::Path) -> Option<String> {
    let needle = format!(" on {} (hfs", mount_point.display());
    mount_output
        .lines()
        .find(|l| l.contains(&needle))
        .and_then(|l| l.split_whitespace().next())
        .map(String::from)
}

/// Whether `device` is a RAM disk: `hdiutil info` lists it under an image whose path is `ram://`.
#[cfg(target_os = "macos")]
fn is_ram_disk(hdiutil_info: &str, device: &str) -> bool {
    hdiutil_info
        .split("================================================")
        .any(|block| {
            block.contains("ram://")
                && block
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(device))
        })
}

#[cfg(target_os = "macos")]
fn make() -> Result<PathBuf, RamDirError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let err = |m: String| RamDirError(m);
    let uid = unsafe { libc::geteuid() };
    let run = |program: &str, args: &[&str]| -> Result<String, RamDirError> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| err(format!("{program}: {e}")))?;
        if !out.status.success() {
            return Err(err(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // Two processes starting together must not both attach a disk.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(std::env::temp_dir().join(format!("nymira-wallet-{uid}.lock")))
        .map_err(|e| err(e.to_string()))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(err(std::io::Error::last_os_error().to_string()));
    }

    // The mount point is a directory only this user can enter, in the per-user temporary
    // directory; `mount` prints its real path.
    let wanted = std::env::temp_dir().join(format!("nymira-wallet-{uid}"));
    std::fs::create_dir_all(&wanted).map_err(|e| err(e.to_string()))?;
    std::fs::set_permissions(&wanted, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| err(e.to_string()))?;
    let mount_point = wanted.canonicalize().map_err(|e| err(e.to_string()))?;

    // A volume left by an earlier run (or a crash) is reused, after checking it is memory.
    if let Some(device) = mounted_device(&run("/sbin/mount", &[])?, &mount_point) {
        if !is_ram_disk(&run("/usr/bin/hdiutil", &["info"])?, &device) {
            return Err(err(format!(
                "{} is mounted from {device}, which is not a RAM disk",
                mount_point.display()
            )));
        }
        return Ok(mount_point);
    }
    // 64 MB: the wallet keeps a few small JSON files and proofs. `nobrowse` keeps the volume out
    // of Finder and Spotlight; ownership stays on, so the 0700 mode holds.
    let device = run("/usr/bin/hdiutil", &["attach", "-nomount", "ram://131072"])?
        .split_whitespace()
        .next()
        .ok_or_else(|| err("hdiutil printed no device".into()))?
        .to_string();
    let mounted = run(
        "/sbin/newfs_hfs",
        &[
            "-v",
            "nymira-wallet",
            &device.replacen("/dev/disk", "/dev/rdisk", 1),
        ],
    )
    .and_then(|_| {
        run(
            "/sbin/mount",
            &[
                "-t",
                "hfs",
                "-o",
                "nobrowse,nodev,nosuid",
                &device,
                &mount_point.to_string_lossy(),
            ],
        )
    });
    if let Err(e) = mounted {
        let _ = run("/usr/bin/hdiutil", &["detach", &device]);
        return Err(e);
    }
    Ok(mount_point)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn make() -> Result<PathBuf, RamDirError> {
    Err(RamDirError("only Linux and macOS are supported".into()))
}

/// The memory-backed root, made once per process.
pub fn root() -> Result<PathBuf, RamDirError> {
    static ROOT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    ROOT.get_or_init(|| make().map_err(|e| e.0))
        .clone()
        .map_err(RamDirError)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    const INFO: &str = "framework : 600\n================================================\nimage-path      : ram://131072\nimage-alias     : ram://131072\n/dev/disk4          \tApple_partition_scheme\n/dev/disk4s1        \tApple_HFS\t/private/x\n================================================\nimage-path      : /Users/x/a.dmg\n/dev/disk7          \tGUID_partition_scheme\n";

    #[test]
    fn only_a_device_under_a_ram_image_counts() {
        assert!(is_ram_disk(INFO, "/dev/disk4"));
        assert!(!is_ram_disk(INFO, "/dev/disk7"));
        assert!(!is_ram_disk(INFO, "/dev/disk9"));
    }

    #[test]
    fn the_device_of_a_mount_point_is_read_from_mount() {
        let out = "/dev/disk3s1 on / (apfs, local)\n/dev/disk4 on /private/x (hfs, local, nodev, nosuid, nobrowse)\n";
        assert_eq!(
            mounted_device(out, std::path::Path::new("/private/x")).as_deref(),
            Some("/dev/disk4")
        );
        assert_eq!(
            mounted_device(out, std::path::Path::new("/private/y")),
            None
        );
    }
}
