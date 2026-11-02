//! The embedded wallet keeps its state as plain files in a directory. At rest they are sealed
//! under the same passphrase as the note store: the directory is unsealed when a lease starts and
//! sealed again, then removed, when the lease has settled.
//!
//! While a lease is open the files exist in plain form, but only in a memory-backed directory
//! (`ramdir`), never on the disk. A crash leaves them in memory (the directory outlives the
//! process); `seal_leftovers` seals them at the next start. If no memory-backed directory can be
//! made, `checkout` fails and no lease opens.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use nymira_operator::sealed::{SealError, SealedFile};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("wallet state io: {0}")]
    Io(#[from] std::io::Error),
    #[error("wallet state is not valid: {0}")]
    Format(String),
}

pub struct WalletVault {
    home: PathBuf,
    passphrase: Zeroizing<String>,
    rounds: u32,
}

type Files = BTreeMap<String, String>;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl WalletVault {
    pub fn new(home: &Path, passphrase: &str, rounds: u32) -> Self {
        Self {
            home: home.to_path_buf(),
            passphrase: Zeroizing::new(passphrase.to_string()),
            rounds,
        }
    }

    fn sealed(&self, key: &str) -> Result<SealedFile, VaultError> {
        std::fs::create_dir_all(self.home.join("wallets"))?;
        Ok(SealedFile::open(
            &self.home.join("wallets").join(format!("{key}.sealed")),
            &self.passphrase,
            self.rounds,
        )?)
    }

    /// Where this vault's working directories live: in memory, one subdirectory per `home`.
    fn work_root(&self) -> Result<PathBuf, VaultError> {
        let id = hex::encode(&Sha256::digest(self.home.as_os_str().as_encoded_bytes())[..8]);
        Ok(crate::ramdir::root()
            .map_err(|e| VaultError::Format(e.to_string()))?
            .join(id))
    }

    /// The working directory of a note's wallet while it is unsealed.
    pub fn work_dir(&self, key: &str) -> Result<PathBuf, VaultError> {
        Ok(self.work_root()?.join(key))
    }

    /// Unseals the wallet state of `key` into its working directory and returns it. A note
    /// without sealed state gets an empty directory.
    pub fn checkout(&self, key: &str) -> Result<PathBuf, VaultError> {
        let dir = self.work_dir(key)?;
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(self.work_root()?, std::fs::Permissions::from_mode(0o700))?;
        }
        if let Some(bytes) = self.sealed(key)?.read()? {
            let bytes = Zeroizing::new(bytes);
            let files: Files =
                serde_json::from_slice(&bytes).map_err(|e| VaultError::Format(e.to_string()))?;
            for (name, content) in files {
                let path = dir.join(&name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let data = b64()
                    .decode(content)
                    .map_err(|e| VaultError::Format(e.to_string()))?;
                std::fs::write(path, data)?;
            }
        }
        Ok(dir)
    }

    /// Seals the working directory of `key` and removes it.
    pub fn checkin(&self, key: &str) -> Result<(), VaultError> {
        let dir = self.work_dir(key)?;
        let mut files = Files::new();
        collect(&dir, &dir, &mut files)?;
        let bytes = Zeroizing::new(serde_json::to_vec(&files).expect("files serialize"));
        self.sealed(key)?.write(&bytes)?;
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Seals every working directory a crash left behind.
    pub fn seal_leftovers(&self) -> Result<Vec<String>, VaultError> {
        let work = self.work_root()?;
        let Ok(entries) = std::fs::read_dir(&work) else {
            return Ok(Vec::new());
        };
        let mut sealed = Vec::new();
        for entry in entries.flatten() {
            let key = entry.file_name().to_string_lossy().into_owned();
            self.checkin(&key)?;
            sealed.push(key);
        }
        Ok(sealed)
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Files) -> Result<(), VaultError> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else {
            let name = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            out.insert(name, b64().encode(std::fs::read(&path)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_sealed_at_rest_and_comes_back_whole() {
        let home = tempfile::tempdir().unwrap();
        let vault = WalletVault::new(home.path(), "pass", 1_000);
        let dir = vault.checkout("n1").unwrap();
        std::fs::write(dir.join("note_state.json"), br#"{"secret_s":"deadbeef"}"#).unwrap();
        std::fs::create_dir_all(dir.join("request_archive")).unwrap();
        std::fs::write(dir.join("request_archive/a.json"), b"{}").unwrap();
        vault.checkin("n1").unwrap();

        assert!(
            !vault.work_dir("n1").unwrap().exists(),
            "no plain state left"
        );
        let sealed = std::fs::read(home.path().join("wallets/n1.sealed")).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("deadbeef"));

        let dir = vault.checkout("n1").unwrap();
        assert_eq!(
            std::fs::read(dir.join("note_state.json")).unwrap(),
            br#"{"secret_s":"deadbeef"}"#
        );
        assert!(dir.join("request_archive/a.json").exists());
        // A wrong passphrase opens nothing.
        vault.checkin("n1").unwrap();
        let wrong = WalletVault::new(home.path(), "other", 1_000);
        assert!(wrong.checkout("n1").is_err());
    }

    #[test]
    fn open_state_is_in_memory_and_never_under_home() {
        let home = tempfile::tempdir().unwrap();
        let vault = WalletVault::new(home.path(), "pass", 1_000);
        let dir = vault.checkout("n3").unwrap();
        std::fs::write(dir.join("note_state.json"), br#"{"secret_s":"cafe01"}"#).unwrap();
        assert!(!dir.starts_with(home.path()));
        assert!(!dir.starts_with(std::env::temp_dir()));
        for entry in std::fs::read_dir(home.path()).unwrap().flatten() {
            let text = String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap_or_default())
                .into_owned();
            assert!(
                !text.contains("cafe01"),
                "plain state in {:?}",
                entry.path()
            );
        }
        vault.checkin("n3").unwrap();
    }

    /// The process dies with a lease open (the vault value is dropped without `checkin`); the next
    /// start finds the state in memory, seals it, and the note's wallet comes back whole.
    #[test]
    fn a_crash_in_the_middle_of_a_lease_recovers_at_the_next_start() {
        let home = tempfile::tempdir().unwrap();
        let first = WalletVault::new(home.path(), "pass", 1_000);
        let dir = first.checkout("n4").unwrap();
        std::fs::write(dir.join("note_state.json"), br#"{"secret_s":"beef02"}"#).unwrap();
        drop(first);

        let next = WalletVault::new(home.path(), "pass", 1_000);
        assert_eq!(next.seal_leftovers().unwrap(), vec!["n4".to_string()]);
        assert!(!dir.exists());
        let sealed = std::fs::read(home.path().join("wallets/n4.sealed")).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("beef02"));
        let back = next.checkout("n4").unwrap();
        assert_eq!(
            std::fs::read(back.join("note_state.json")).unwrap(),
            br#"{"secret_s":"beef02"}"#
        );
        next.checkin("n4").unwrap();
    }

    #[test]
    fn a_crash_leftover_is_sealed_at_start() {
        let home = tempfile::tempdir().unwrap();
        let vault = WalletVault::new(home.path(), "pass", 1_000);
        let dir = vault.checkout("n2").unwrap();
        std::fs::write(dir.join("note_state.json"), b"{\"x\":1}").unwrap();
        assert_eq!(vault.seal_leftovers().unwrap(), vec!["n2".to_string()]);
        assert!(!vault.work_dir("n2").unwrap().exists());
        assert!(home.path().join("wallets/n2.sealed").exists());
    }
}
