//! Encrypted backups: age X25519, to a recipient public key chosen at setup. The private key is
//! shown once at generation and is never stored by `nymirad`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup io: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup recipient is not an age public key")]
    Recipient,
    #[error("backup encryption failed: {0}")]
    Crypt(String),
}

/// A new key pair: the private key to show once, and the public key to keep.
pub fn generate_identity() -> (Zeroizing<String>, String) {
    let identity = age::x25519::Identity::generate();
    (
        Zeroizing::new(identity.to_string().expose_secret().to_string()),
        identity.to_public().to_string(),
    )
}

pub fn parse_recipient(text: &str) -> Result<age::x25519::Recipient, BackupError> {
    text.trim().parse().map_err(|_| BackupError::Recipient)
}

pub fn encrypt(
    recipient: &age::x25519::Recipient,
    plaintext: &[u8],
) -> Result<Vec<u8>, BackupError> {
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient))
            .map_err(|e| BackupError::Crypt(e.to_string()))?;
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|e| BackupError::Crypt(e.to_string()))?;
    writer.write_all(plaintext)?;
    writer
        .finish()
        .map_err(|e| BackupError::Crypt(e.to_string()))?;
    Ok(out)
}

/// Opens a backup with the private key. Used by restore and by tests.
pub fn decrypt(identity: &str, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, BackupError> {
    let identity: age::x25519::Identity = identity
        .trim()
        .parse()
        .map_err(|_| BackupError::Crypt("not an age private key".into()))?;
    let decryptor =
        age::Decryptor::new(ciphertext).map_err(|e| BackupError::Crypt(e.to_string()))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| BackupError::Crypt(e.to_string()))?;
    let mut out = Zeroizing::new(Vec::new());
    reader.read_to_end(&mut out)?;
    Ok(out)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}

/// Encrypts `plaintext` once and writes the same file into every directory in `places`.
/// Returns the paths written.
pub fn write_backups(
    recipient: &age::x25519::Recipient,
    plaintext: &[u8],
    places: &[PathBuf],
    name: &str,
) -> Result<Vec<PathBuf>, BackupError> {
    let ciphertext = encrypt(recipient, plaintext)?;
    let mut written = Vec::new();
    for dir in places {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        write_private(&path, &ciphertext)?;
        written.push(path);
    }
    Ok(written)
}
