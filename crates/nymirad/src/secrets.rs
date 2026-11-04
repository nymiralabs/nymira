//! Note secrets: made per note with the chain label in the derivation, and kept only in a file
//! sealed under a passphrase. Passphrases come from the terminal, stdin or a 0600 file, never
//! from `argv` or the environment.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};
use zkapi_types::Felt252;

use nymira_operator::sealed::{SealError, SealedFile};

pub const ROUNDS: u32 = 600_000;
const DOMAIN: &[u8] = b"nymira/note-secret/v1\0";

/// A field element below the modulus: 31 bytes of `SHA-256(domain | chain label | entropy)`.
pub fn derive_secret(chain_label: &str, entropy: &[u8; 32]) -> Felt252 {
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(chain_label.as_bytes());
    h.update([0]);
    h.update(entropy);
    let digest = h.finalize();
    let mut word = [0u8; 32];
    word[1..].copy_from_slice(&digest[..31]);
    Felt252(word)
}

/// A fresh secret for a note on `chain_label`, with its registration commitment.
pub fn new_secret(chain_label: &str) -> (Felt252, Felt252) {
    let mut entropy = Zeroizing::new([0u8; 32]);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut *entropy);
    let secret = derive_secret(chain_label, &entropy);
    (secret, zkapi_core::v2::registration_commitment(&secret))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    /// The secret is stored; the transfer may or may not have gone out.
    Pending,
    /// The vault's `deposit` event matches what was sent.
    Accepted,
    /// The transfer failed or the vault refunded it.
    Failed,
    /// `request_exit` was filed or its proof may have left: no new request on this note.
    Exiting,
    /// The vault paid the note out; its wallet is archived.
    Closed,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NoteEntry {
    pub chain_label: String,
    /// The secret, 64 hex digits. Never printed.
    pub secret: String,
    pub commitment: String,
    /// The denomination sent, in token base units.
    pub denomination: String,
    pub payout: String,
    pub status: NoteStatus,
    pub note_id: Option<u32>,
    pub expiry_ns: Option<u64>,
    pub deposit_amount: Option<String>,
    pub tx_hash: Option<String>,
    pub created_at: u64,
}

impl std::fmt::Debug for NoteEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoteEntry")
            .field("commitment", &self.commitment)
            .field("status", &self.status)
            .field("note_id", &self.note_id)
            .finish_non_exhaustive()
    }
}

impl Drop for NoteEntry {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("the note file is not valid: {0}")]
    Format(String),
}

/// The sealed list of notes.
pub struct NoteStore {
    file: SealedFile,
    funding: SealedFile,
    lock_path: PathBuf,
}

impl NoteStore {
    pub fn open(path: &Path, passphrase: &str, rounds: u32) -> Result<Self, StoreError> {
        let file = SealedFile::open(path, passphrase, rounds)?;
        file.read()?; // a wrong passphrase fails here, not at the first write
        let mut funding_path = path.as_os_str().to_owned();
        funding_path.push(".funding");
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let funding = SealedFile::open(Path::new(&funding_path), passphrase, rounds)?;
        Ok(Self {
            file,
            funding,
            lock_path: lock_path.into(),
        })
    }

    pub fn list(&self) -> Result<Vec<NoteEntry>, StoreError> {
        match self.file.read()? {
            None => Ok(Vec::new()),
            Some(bytes) => {
                let bytes = Zeroizing::new(bytes);
                serde_json::from_slice(&bytes).map_err(|e| StoreError::Format(e.to_string()))
            }
        }
    }

    fn lock(&self) -> Result<std::fs::File, StoreError> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&self.lock_path)
            .map_err(|e| StoreError::Format(e.to_string()))?;
        // SAFETY: the descriptor is live for the call; closing the returned file releases the lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(StoreError::Format(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(file)
    }

    pub fn funding_attempts(&self) -> Result<Vec<crate::intents::Attempt>, StoreError> {
        match self.funding.read()? {
            None => Ok(vec![]),
            Some(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))
                .map_err(|e| StoreError::Format(e.to_string())),
        }
    }
    pub fn restore_funding(&self, attempts: &[crate::intents::Attempt]) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        self.write_funding(attempts)
    }
    pub fn save_funding(&self, attempt: &crate::intents::Attempt) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let mut attempts = self.funding_attempts()?;
        attempts.retain(|a| a.commitment != attempt.commitment);
        attempts.push(attempt.clone());
        self.write_funding(&attempts)
    }
    fn write_funding(&self, attempts: &[crate::intents::Attempt]) -> Result<(), StoreError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(attempts).map_err(|e| StoreError::Format(e.to_string()))?,
        );
        Ok(self.funding.write(&bytes)?)
    }
    pub fn save(&self, notes: &[NoteEntry]) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        self.write_notes(notes)
    }
    fn write_notes(&self, notes: &[NoteEntry]) -> Result<(), StoreError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(notes).map_err(|e| StoreError::Format(e.to_string()))?,
        );
        Ok(self.file.write(&bytes)?)
    }

    /// Adds one entry and writes the file (synced) before returning.
    pub fn add(&self, entry: NoteEntry) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let mut notes = self.list()?;
        notes.push(entry);
        self.write_notes(&notes)
    }

    /// Changes the entry with `commitment`.
    pub fn update(
        &self,
        commitment: &str,
        change: impl FnOnce(&mut NoteEntry),
    ) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let mut notes = self.list()?;
        let entry = notes
            .iter_mut()
            .find(|n| n.commitment == commitment)
            .ok_or_else(|| StoreError::Format("no such note".into()))?;
        change(entry);
        self.write_notes(&notes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PassphraseSource {
    /// Read from the controlling terminal without echo.
    Terminal,
    /// The first line of stdin.
    Stdin,
    /// The first line of a file that only its owner can read.
    File(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum PassphraseError {
    #[error("passphrase io: {0}")]
    Io(#[from] std::io::Error),
    #[error("the passphrase file is readable by others (mode {0:o})")]
    Mode(u32),
    #[error("the passphrase is empty")]
    Empty,
}

impl PassphraseSource {
    pub fn read(&self) -> Result<Zeroizing<String>, PassphraseError> {
        let line = match self {
            Self::Terminal => rpassword::prompt_password("Passphrase: ")?,
            Self::Stdin => {
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                line
            }
            Self::File(path) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        return Err(PassphraseError::Mode(mode));
                    }
                }
                std::fs::read_to_string(path)?
            }
        };
        let line = Zeroizing::new(line);
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return Err(PassphraseError::Empty);
        }
        Ok(Zeroizing::new(trimmed.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn file_with(mode: u32, text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pass");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path)
    }

    #[test]
    fn a_passphrase_file_others_can_read_is_refused() {
        for mode in [0o644, 0o640, 0o604, 0o666] {
            let (_dir, path) = file_with(mode, "hunter2hunter2\n");
            match PassphraseSource::File(path).read() {
                Err(PassphraseError::Mode(m)) => assert_eq!(m, mode),
                other => panic!("mode {mode:o}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_private_passphrase_file_is_read_without_its_line_ending() {
        for mode in [0o600, 0o400] {
            let (_dir, path) = file_with(mode, "correct horse\r\n");
            let pass = PassphraseSource::File(path).read().unwrap();
            assert_eq!(pass.as_str(), "correct horse");
        }
        let (_dir, empty) = file_with(0o600, "\n");
        assert!(matches!(
            PassphraseSource::File(empty).read(),
            Err(PassphraseError::Empty)
        ));
    }

    #[test]
    fn the_chain_label_and_the_entropy_both_change_the_secret() {
        let entropy = [7u8; 32];
        let testnet = derive_secret("near-testnet", &entropy);
        assert_eq!(testnet, derive_secret("near-testnet", &entropy));
        assert_ne!(testnet, derive_secret("near-mainnet", &entropy));
        assert_ne!(testnet, derive_secret("near-testnet", &[8u8; 32]));
        // The label is delimited: moving a byte between label and entropy changes the result.
        let mut shifted = entropy;
        shifted[0] = b'x';
        assert_ne!(
            derive_secret("near-testnet", &shifted),
            derive_secret("near-testnetx", &entropy)
        );
        // A field element below the modulus: the top byte is zero.
        assert_eq!(testnet.0[0], 0);
    }

    #[test]
    fn a_new_secret_comes_with_its_registration_commitment() {
        let (secret, commitment) = new_secret("near-testnet");
        assert_eq!(commitment, zkapi_core::v2::registration_commitment(&secret));
        assert_ne!(new_secret("near-testnet").0, secret);
    }
}
