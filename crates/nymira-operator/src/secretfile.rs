//! Secrets on disk and in the terminal.
//!
//! Each secret sits in its own file with mode 0400, owned by the user the process runs as, in a
//! directory only that user can enter. A file that anyone else could read or write is refused.
//! Passphrases come from a prompt or a pipe on standard input, never from the environment.

use std::io::{BufRead, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret file: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} must be a regular file with mode 0400, owned by this user")]
    Unsafe(String),
    #[error("the passphrase is empty")]
    Empty,
}

fn euid() -> u32 {
    // SAFETY: no arguments, no failure mode.
    unsafe { libc::geteuid() }
}

/// Writes a new secret file, mode 0400. It refuses to replace an existing file: rotate by
/// writing the new one beside it and moving it in.
pub fn write(path: &Path, secret: &[u8]) -> Result<(), SecretError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)?;
    file.write_all(secret)?;
    file.sync_all()?;
    Ok(())
}

/// Reads a secret file after checking its mode and owner.
pub fn read(path: &Path) -> Result<Zeroizing<Vec<u8>>, SecretError> {
    let meta = std::fs::symlink_metadata(path)?;
    let unsafe_file = || SecretError::Unsafe(path.display().to_string());
    if !meta.is_file() || meta.mode() & 0o377 != 0 || meta.uid() != euid() {
        return Err(unsafe_file());
    }
    Ok(Zeroizing::new(std::fs::read(path)?))
}

/// One passphrase line from `input`, without its line end.
pub fn passphrase_from(mut input: impl BufRead) -> Result<Zeroizing<String>, SecretError> {
    let mut line = Zeroizing::new(String::new());
    input.read_line(&mut line)?;
    let trimmed = line.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Err(SecretError::Empty);
    }
    Ok(Zeroizing::new(trimmed.to_string()))
}

/// Asks for a passphrase on the terminal when there is one, else reads a line from standard
/// input (a pipe from the person starting the service). The environment is never read.
pub fn passphrase(label: &str) -> Result<Zeroizing<String>, SecretError> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        let line = Zeroizing::new(rpassword::prompt_password(format!("{label}: "))?);
        if line.is_empty() {
            return Err(SecretError::Empty);
        }
        return Ok(line);
    }
    passphrase_from(std::io::stdin().lock())
}
