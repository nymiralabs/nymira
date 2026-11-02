//! A directory that lives in memory, for the wallet's working files.
//!
//! The embedded wallet reads and writes its state as plain files and cannot be pointed at a
//! buffer, so while a lease is open those files must sit somewhere that is not a disk. On Linux
//! that is a tmpfs directory under `/dev/shm`; on macOS a RAM disk made with `hdiutil` and
//! `diskutil` (both work for an ordinary user). If neither can be made, or the directory turns out
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

#[cfg(target_os = "macos")]
fn make() -> Result<PathBuf, RamDirError> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let name = format!("nymira-wallet-{}", unsafe { libc::geteuid() });
    let mount = PathBuf::from("/Volumes").join(&name);
    let run = |program: &str, args: &[&str]| -> Result<String, RamDirError> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| RamDirError(format!("{program}: {e}")))?;
        if !out.status.success() {
            return Err(RamDirError(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // A volume left by an earlier run (or a crash) is reused: its files are sealed at start.
    let mounted = Command::new("/sbin/mount")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).contains(&format!(" on {} (hfs", mount.display()))
        })
        .unwrap_or(false);
    if !mounted {
        // 64 MB: the wallet keeps a few small JSON files and proofs.
        let device = run("/usr/bin/hdiutil", &["attach", "-nomount", "ram://131072"])?
            .split_whitespace()
            .next()
            .ok_or_else(|| RamDirError("hdiutil printed no device".into()))?
            .to_string();
        run(
            "/usr/sbin/diskutil",
            &["erasevolume", "HFS+", &name, &device],
        )?;
    }
    std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| RamDirError(e.to_string()))?;
    Ok(mount)
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
