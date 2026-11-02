//! A small file the operator keeps secret: the NEAR AI refresh token and the key table.
//!
//! The file is `NYMSEAL1 | salt (16) | nonce (24) | ciphertext`, XChaCha20-Poly1305 under a key
//! from the passphrase (PBKDF2-HMAC-SHA256). A write goes to a sibling file with mode 0600, is
//! synced, then renamed over the target, so a crash leaves the old content or the new, never half.

use std::io::Write;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;

const MAGIC: &[u8; 8] = b"NYMSEAL1";

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("sealed file io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sealed file is damaged or the passphrase is wrong")]
    Damaged,
}

pub struct SealedFile {
    path: PathBuf,
    salt: [u8; 16],
    key: [u8; 32],
}

impl SealedFile {
    /// Opens `path`; the salt of an existing file is kept, a new file gets a random one.
    pub fn open(path: &Path, passphrase: &str, rounds: u32) -> Result<Self, SealError> {
        let mut salt = [0u8; 16];
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() >= 8 + 16 && bytes.starts_with(MAGIC) => {
                salt.copy_from_slice(&bytes[8..24]);
            }
            Ok(_) => return Err(SealError::Damaged),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                rand::thread_rng().fill_bytes(&mut salt);
            }
            Err(e) => return Err(e.into()),
        }
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(passphrase.as_bytes(), &salt, rounds, &mut key);
        Ok(Self {
            path: path.to_path_buf(),
            salt,
            key,
        })
    }

    /// The content, or `None` when the file does not exist yet.
    pub fn read(&self) -> Result<Option<Vec<u8>>, SealError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if bytes.len() < 8 + 16 + 24 || !bytes.starts_with(MAGIC) || bytes[8..24] != self.salt {
            return Err(SealError::Damaged);
        }
        let (nonce, ct) = bytes[24..].split_at(24);
        XChaCha20Poly1305::new((&self.key).into())
            .decrypt(XNonce::from_slice(nonce), ct)
            .map(Some)
            .map_err(|_| SealError::Damaged)
    }

    pub fn write(&self, plaintext: &[u8]) -> Result<(), SealError> {
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = XChaCha20Poly1305::new((&self.key).into())
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| SealError::Damaged)?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(MAGIC)?;
        file.write_all(&self.salt)?;
        file.write_all(&nonce)?;
        file.write_all(&ct)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_reads_back_and_a_wrong_passphrase_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.enc");
        let file = SealedFile::open(&path, "pass", 10).unwrap();
        assert_eq!(file.read().unwrap(), None);
        file.write(b"rt-secret").unwrap();
        file.write(b"rt-secret-2").unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"secret"));
        let again = SealedFile::open(&path, "pass", 10).unwrap();
        assert_eq!(again.read().unwrap().as_deref(), Some(&b"rt-secret-2"[..]));
        let wrong = SealedFile::open(&path, "other", 10).unwrap();
        assert!(matches!(wrong.read(), Err(SealError::Damaged)));
        assert!(!dir.path().join("s.enc.tmp").exists());
    }
}
