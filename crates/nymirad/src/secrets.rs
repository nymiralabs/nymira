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
}

impl NoteStore {
    pub fn open(path: &Path, passphrase: &str, rounds: u32) -> Result<Self, StoreError> {
        let file = SealedFile::open(path, passphrase, rounds)?;
        file.read()?; // a wrong passphrase fails here, not at the first write
        Ok(Self { file })
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

    pub fn save(&self, notes: &[NoteEntry]) -> Result<(), StoreError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(notes).map_err(|e| StoreError::Format(e.to_string()))?,
        );
        Ok(self.file.write(&bytes)?)
    }

    /// Adds one entry and writes the file (synced) before returning.
    pub fn add(&self, entry: NoteEntry) -> Result<(), StoreError> {
        let mut notes = self.list()?;
        notes.push(entry);
        self.save(&notes)
    }

    /// Changes the entry with `commitment`.
    pub fn update(
        &self,
        commitment: &str,
        change: impl FnOnce(&mut NoteEntry),
    ) -> Result<(), StoreError> {
        let mut notes = self.list()?;
        let entry = notes
            .iter_mut()
            .find(|n| n.commitment == commitment)
            .ok_or_else(|| StoreError::Format("no such note".into()))?;
        change(entry);
        self.save(&notes)
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
