//! Submitter accounts: the NEAR accounts that sign close and exit transactions, so the funding
//! account never does. Each has a function-call key limited to [`METHODS`] on the vault; its
//! seed sits in a sealed file next to the notes. A family names the notes that share one
//! submitter (the personal notes, or one budget note).

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::SigningKey;
use nymira_operator::sealed::{SealError, SealedFile};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The only methods a submitter key may call; the account's access key is limited to these.
pub const METHODS: [&str; 4] = [
    "request_exit",
    "finalize_exit",
    "mutual_close",
    "claim_payout",
];

/// The family a note belongs to until the user assigns another.
pub const PERSONAL: &str = "personal";

#[derive(Debug, thiserror::Error)]
pub enum SubmitterError {
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error("the submitter file is not valid: {0}")]
    Format(String),
    #[error("a submitter for family {0:?} exists already; use another family name")]
    Exists(String),
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    family: String,
    account: String,
    /// The ed25519 seed, 64 hex digits.
    seed: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Contents {
    submitters: Vec<Entry>,
    /// Note commitment to family.
    assignments: BTreeMap<String, String>,
}

/// A submitter ready to sign.
pub struct Submitter {
    pub family: String,
    pub account: String,
    pub key: SigningKey,
}

impl Submitter {
    /// `ed25519:` and the base58 public key, as NEAR prints it.
    pub fn public_key(&self) -> String {
        public_key(&self.key)
    }
}

fn public_key(key: &SigningKey) -> String {
    format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
}

pub struct SubmitterStore {
    file: SealedFile,
}

impl SubmitterStore {
    pub fn open(path: &Path, passphrase: &str, rounds: u32) -> Result<Self, SubmitterError> {
        let file = SealedFile::open(path, passphrase, rounds)?;
        file.read()?;
        Ok(Self { file })
    }

    fn load(&self) -> Result<Contents, SubmitterError> {
        match self.file.read()? {
            None => Ok(Contents::default()),
            Some(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))
                .map_err(|e| SubmitterError::Format(e.to_string())),
        }
    }

    fn save(&self, contents: &Contents) -> Result<(), SubmitterError> {
        let bytes = Zeroizing::new(
            serde_json::to_vec(contents).map_err(|e| SubmitterError::Format(e.to_string()))?,
        );
        Ok(self.file.write(&bytes)?)
    }

    /// The whole store as JSON, for the backup.
    pub fn export_json(&self) -> Result<Zeroizing<Vec<u8>>, SubmitterError> {
        Ok(Zeroizing::new(
            serde_json::to_vec(&self.load()?).map_err(|e| SubmitterError::Format(e.to_string()))?,
        ))
    }

    /// Replaces the store with the JSON a backup carried.
    pub fn import_json(&self, json: &[u8]) -> Result<(), SubmitterError> {
        let contents: Contents =
            serde_json::from_slice(json).map_err(|e| SubmitterError::Format(e.to_string()))?;
        self.save(&contents)
    }

    /// Makes a key for `account` under `family`. The user adds its public key to the account
    /// with the access the printed instructions name.
    pub fn add(&self, family: &str, account: &str) -> Result<Submitter, SubmitterError> {
        let mut contents = self.load()?;
        if contents.submitters.iter().any(|s| s.family == family) {
            return Err(SubmitterError::Exists(family.into()));
        }
        let seed = Zeroizing::new(rand::random::<[u8; 32]>());
        contents.submitters.push(Entry {
            family: family.into(),
            account: account.into(),
            seed: hex::encode(*seed),
        });
        self.save(&contents)?;
        Ok(Submitter {
            family: family.into(),
            account: account.into(),
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// The submitter of `family`, if one was made.
    pub fn get(&self, family: &str) -> Result<Option<Submitter>, SubmitterError> {
        let contents = self.load()?;
        contents
            .submitters
            .iter()
            .find(|s| s.family == family)
            .map(|e| {
                let seed: [u8; 32] = hex::decode(&e.seed)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| SubmitterError::Format("a seed is not 32 bytes".into()))?;
                Ok(Submitter {
                    family: e.family.clone(),
                    account: e.account.clone(),
                    key: SigningKey::from_bytes(&seed),
                })
            })
            .transpose()
    }

    /// The families that have a submitter, with their accounts.
    pub fn list(&self) -> Result<Vec<(String, String)>, SubmitterError> {
        Ok(self
            .load()?
            .submitters
            .into_iter()
            .map(|s| (s.family, s.account))
            .collect())
    }

    /// Puts a note in `family`.
    pub fn assign(&self, commitment: &str, family: &str) -> Result<(), SubmitterError> {
        let mut contents = self.load()?;
        contents
            .assignments
            .insert(commitment.into(), family.into());
        self.save(&contents)
    }

    /// The family of a note: the assigned one, else [`PERSONAL`].
    pub fn family_of(&self, commitment: &str) -> Result<String, SubmitterError> {
        Ok(self
            .load()?
            .assignments
            .get(commitment)
            .cloned()
            .unwrap_or_else(|| PERSONAL.into()))
    }
}

/// What the user does with a new submitter account, in words.
pub fn instructions(s: &Submitter, vault: &str) -> String {
    format!(
        "Submitter {:?}: account {}\nPublic key {}\nAdd this key to the account as a function-call key for contract {} with the methods {}, then send the account a small amount of NEAR for gas. The account holds no other key and no funds beyond that.",
        s.family,
        s.account,
        s.public_key(),
        vault,
        METHODS.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_has_one_submitter_and_a_note_defaults_to_personal() {
        let dir = tempfile::tempdir().unwrap();
        let store = SubmitterStore::open(&dir.path().join("s.sealed"), "pw", 10).unwrap();
        assert_eq!(store.family_of("0xaa").unwrap(), PERSONAL);
        let made = store.add("personal", "me-sub.testnet").unwrap();
        let again = store.get("personal").unwrap().unwrap();
        assert_eq!(made.public_key(), again.public_key());
        assert!(matches!(
            store.add("personal", "other.testnet"),
            Err(SubmitterError::Exists(_))
        ));
        store.assign("0xaa", "budget-1").unwrap();
        assert_eq!(store.family_of("0xaa").unwrap(), "budget-1");
        assert!(store.get("budget-1").unwrap().is_none());
    }
}
