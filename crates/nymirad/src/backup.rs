//! Encrypted backups of the whole state, and the restore.
//!
//! The backup is age X25519 to a recipient public key the user chose at setup. The private key
//! is shown once, never stored by `nymirad`, and kept off the machine: unattended backups need
//! no secret in memory, and a stolen backup is useless without it.
//!
//! With a passphrase instead (at least [`MIN_PASSPHRASE_CHARS`] characters) the age key pair is
//! derived from it with Argon2id ([`KDF_M_KIB`] KiB of memory, [`KDF_T`] passes, [`KDF_P`] lane,
//! a random 16-byte salt). The daemon keeps only the derived public key and the salt; the
//! passphrase is never stored, so the same holds. A passphrase backup starts with a header line
//! that carries the salt and the parameters, so it restores from the file alone.
//!
//! Inside the encryption the backup holds the notes, the wallet states and the other state
//! files in the clear (they are sealed on disk under the machine's passphrase, which a restore
//! on a new machine cannot assume). That is why the key must stay off the machine.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use base64::Engine;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secrets::{NoteEntry, NoteStore, StoreError};
use crate::submitter::SubmitterStore;
use crate::walletvault::{Files, WalletVault};

pub const MIN_PASSPHRASE_CHARS: usize = 12;
/// Argon2id parameters for a backup passphrase.
pub const KDF_M_KIB: u32 = 65_536;
pub const KDF_T: u32 = 3;
pub const KDF_P: u32 = 1;
const KDF_SALT_BYTES: usize = 16;
const HEADER: &str = "NYMIRA-BACKUP-KDF1 ";
const KEY_PREFIX: &str = "AGE-SECRET-KEY-1";

/// What the backup file is called in each place.
pub const STATE_FILE: &str = "state.age";
/// What a deposit writes before any lease exists: the notes alone.
pub const NOTES_FILE: &str = "notes.age";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup io: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup recipient is not an age public key")]
    Recipient,
    #[error("backup encryption failed: {0}")]
    Crypt(String),
    #[error("the state could not be read for the backup: {0}")]
    State(String),
    #[error("the backup passphrase must have at least {MIN_PASSPHRASE_CHARS} characters")]
    ShortPassphrase,
    #[error("the backup was not written to every place: {}", .0.iter().map(|(p, e)| format!("{}: {e}", p.display())).collect::<Vec<_>>().join("; "))]
    Places(Vec<(PathBuf, String)>),
}

/// The public part of a passphrase backup: how the key was derived. Not secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// 16 random bytes, hex.
    pub salt: String,
}

impl Kdf {
    pub fn fresh() -> Self {
        Self {
            alg: "argon2id".into(),
            m_kib: KDF_M_KIB,
            t: KDF_T,
            p: KDF_P,
            salt: hex::encode(rand::random::<[u8; KDF_SALT_BYTES]>()),
        }
    }

    /// The age identity this passphrase stands for.
    pub fn identity(&self, passphrase: &str) -> Result<age::x25519::Identity, BackupError> {
        use bech32::ToBase32;
        if self.alg != "argon2id" {
            return Err(BackupError::Crypt(format!("unknown kdf {}", self.alg)));
        }
        let salt = hex::decode(&self.salt).map_err(|e| BackupError::Crypt(e.to_string()))?;
        let params = argon2::Params::new(self.m_kib, self.t, self.p, Some(32))
            .map_err(|e| BackupError::Crypt(e.to_string()))?;
        let mut seed = Zeroizing::new([0u8; 32]);
        argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, &mut *seed)
            .map_err(|e| BackupError::Crypt(e.to_string()))?;
        let text = Zeroizing::new(
            bech32::encode("age-secret-key-", seed.to_base32(), bech32::Variant::Bech32)
                .map_err(|e| BackupError::Crypt(e.to_string()))?
                .to_uppercase(),
        );
        text.parse()
            .map_err(|_| BackupError::Crypt("derived key is not an age key".into()))
    }
}

/// Where backups go and who can read them.
#[derive(Clone)]
pub struct Target {
    pub recipient: age::x25519::Recipient,
    pub places: Vec<PathBuf>,
    /// Set for a passphrase backup.
    pub kdf: Option<Kdf>,
}

impl Target {
    pub fn new(recipient: age::x25519::Recipient, places: Vec<PathBuf>) -> Self {
        Self {
            recipient,
            places,
            kdf: None,
        }
    }

    pub fn with_kdf(mut self, kdf: Kdf) -> Self {
        self.kdf = Some(kdf);
        self
    }

    /// From `config.json` values. `None` when no recipient is set or it does not parse.
    pub fn from_config(
        recipient: Option<&str>,
        places: &[PathBuf],
        kdf: &Option<Kdf>,
    ) -> Option<Self> {
        let recipient = parse_recipient(recipient?).ok()?;
        Some(Self {
            recipient,
            places: places.to_vec(),
            kdf: kdf.clone(),
        })
    }
}

/// A new key pair: the private key to show once, and the public key to keep.
pub fn generate_identity() -> (Zeroizing<String>, String) {
    let identity = age::x25519::Identity::generate();
    (
        Zeroizing::new(identity.to_string().expose_secret().to_string()),
        identity.to_public().to_string(),
    )
}

/// A passphrase backup: the derivation parameters to keep and the public key it gives.
pub fn passphrase_setup(passphrase: &str) -> Result<(Kdf, String), BackupError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(BackupError::ShortPassphrase);
    }
    let kdf = Kdf::fresh();
    let public = kdf.identity(passphrase)?.to_public().to_string();
    Ok((kdf, public))
}

pub fn parse_recipient(text: &str) -> Result<age::x25519::Recipient, BackupError> {
    text.trim().parse().map_err(|_| BackupError::Recipient)
}

pub fn encrypt(target: &Target, plaintext: &[u8]) -> Result<Vec<u8>, BackupError> {
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&target.recipient as &dyn age::Recipient))
            .map_err(|e| BackupError::Crypt(e.to_string()))?;
    let mut out = Vec::new();
    if let Some(kdf) = &target.kdf {
        out.extend_from_slice(HEADER.as_bytes());
        out.extend_from_slice(&serde_json::to_vec(kdf).expect("kdf serializes"));
        out.push(b'\n');
    }
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|e| BackupError::Crypt(e.to_string()))?;
    writer.write_all(plaintext)?;
    writer
        .finish()
        .map_err(|e| BackupError::Crypt(e.to_string()))?;
    Ok(out)
}

/// The header of a passphrase backup and the age bytes after it.
fn split(file: &[u8]) -> Result<(Option<Kdf>, &[u8]), BackupError> {
    let Some(rest) = file.strip_prefix(HEADER.as_bytes()) else {
        return Ok((None, file));
    };
    let end = rest
        .iter()
        .position(|b| *b == b'\n')
        .ok_or_else(|| BackupError::Crypt("the backup header is cut off".into()))?;
    let kdf = serde_json::from_slice(&rest[..end])
        .map_err(|e| BackupError::Crypt(format!("the backup header: {e}")))?;
    Ok((Some(kdf), &rest[end + 1..]))
}

/// Opens a backup with what the user holds: the age private key, or the passphrase of a
/// passphrase backup. Used by restore and by tests.
pub fn decrypt(secret: &str, file: &[u8]) -> Result<Zeroizing<Vec<u8>>, BackupError> {
    let (kdf, ciphertext) = split(file)?;
    let secret = secret.trim();
    let identity: age::x25519::Identity = if secret.to_uppercase().starts_with(KEY_PREFIX) {
        secret
            .parse()
            .map_err(|_| BackupError::Crypt("not an age private key".into()))?
    } else {
        kdf.ok_or_else(|| {
            BackupError::Crypt(
                "this backup was made for an age key, not a passphrase: give the private key"
                    .into(),
            )
        })?
        .identity(secret)?
    };
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

/// Encrypts `plaintext` once and writes the same file into every place. Every place is tried:
/// one that fails does not stop the others, and the error names each one that failed.
/// Returns the paths written.
pub fn write_backups(
    target: &Target,
    plaintext: &[u8],
    name: &str,
) -> Result<Vec<PathBuf>, BackupError> {
    let ciphertext = encrypt(target, plaintext)?;
    let (mut written, mut failed) = (Vec::new(), Vec::new());
    for dir in &target.places {
        let path = dir.join(name);
        match std::fs::create_dir_all(dir).and_then(|_| write_private(&path, &ciphertext)) {
            Ok(()) => written.push(path),
            Err(e) => failed.push((dir.clone(), e.to_string())),
        }
    }
    if failed.is_empty() {
        Ok(written)
    } else {
        Err(BackupError::Places(failed))
    }
}

/// Everything a restore needs, in one document.
#[derive(Default, Serialize, Deserialize)]
pub struct StateBackup {
    pub version: u32,
    /// Unix seconds the backup was taken.
    pub taken_at: u64,
    pub notes: Vec<NoteEntry>,
    #[serde(default)]
    pub funding: Vec<crate::intents::Attempt>,
    /// Commitment to the wallet's files (name to base64).
    pub wallets: BTreeMap<String, Files>,
    /// The submitter store's contents, as the JSON the store writes.
    pub submitters: Option<String>,
    /// Plain state files by name (`exits.json`, `lease-records.jsonl`, `config.json`), base64.
    pub files: BTreeMap<String, String>,
}

pub const VERSION: u32 = 1;

impl StateBackup {
    pub fn to_bytes(&self) -> Result<Zeroizing<Vec<u8>>, BackupError> {
        serde_json::to_vec(self)
            .map(Zeroizing::new)
            .map_err(|e| BackupError::Crypt(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BackupError> {
        let backup: Self = serde_json::from_slice(bytes)
            .map_err(|e| BackupError::Crypt(format!("not a nymirad backup: {e}")))?;
        if backup.version != VERSION {
            return Err(BackupError::Crypt(format!(
                "backup version {} is not one this build reads",
                backup.version
            )));
        }
        Ok(backup)
    }

    /// Adds a plain file from the state directory if it exists.
    pub fn add_file(&mut self, home: &Path, name: &str) {
        if let Ok(bytes) = std::fs::read(home.join(name)) {
            self.files.insert(
                name.into(),
                base64::engine::general_purpose::STANDARD.encode(bytes),
            );
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error(transparent)]
    Backup(#[from] BackupError),
    #[error("{0} already holds state; restore into an empty directory (move the old one away first): restoring an older state over a newer one would let the operator defeat an exit")]
    StateExists(PathBuf),
    #[error("no backup file found in {0}")]
    NoFile(PathBuf),
    #[error("state io: {0}")]
    State(String),
}

impl From<std::io::Error> for RestoreError {
    fn from(e: std::io::Error) -> Self {
        Self::State(e.to_string())
    }
}

impl From<StoreError> for RestoreError {
    fn from(e: StoreError) -> Self {
        Self::State(e.to_string())
    }
}

/// What a restore brought back.
#[derive(Debug)]
pub struct Restored {
    pub taken_at: u64,
    /// `(note_id, status, has a wallet state)`.
    pub notes: Vec<(Option<u32>, crate::secrets::NoteStatus, bool)>,
    pub files: Vec<PathBuf>,
}

/// Reads the backups at `from` (files, or places that hold `state.age` and `notes.age`) and
/// opens each with `secret`. The newest state backup is the base; notes that only a `notes.age`
/// knows (a deposit made after the last state backup) are added.
pub fn load(from: &[PathBuf], secret: &str) -> Result<StateBackup, RestoreError> {
    let mut states: Vec<StateBackup> = Vec::new();
    let mut extra: Vec<StateBackup> = Vec::new();
    for path in from {
        let candidates = if path.is_dir() {
            vec![
                (path.join(STATE_FILE), true),
                (path.join(NOTES_FILE), false),
            ]
        } else {
            let is_state = path.file_name().is_none_or(|n| n != NOTES_FILE);
            vec![(path.clone(), is_state)]
        };
        let mut found = false;
        for (file, is_state) in candidates {
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            found = true;
            let plain = decrypt(secret, &bytes)?;
            let backup = StateBackup::from_bytes(&plain)?;
            if is_state {
                states.push(backup);
            } else {
                extra.push(backup);
            }
        }
        if !found {
            return Err(RestoreError::NoFile(path.clone()));
        }
    }
    states.sort_by_key(|b| b.taken_at);
    let mut base = match states.pop() {
        Some(b) => b,
        None => extra
            .pop()
            .ok_or_else(|| RestoreError::NoFile(from[0].clone()))?,
    };
    for other in extra {
        for (name, bytes) in other.files {
            base.files.entry(name).or_insert(bytes);
        }
        for attempt in other.funding {
            if !base
                .funding
                .iter()
                .any(|a| a.commitment == attempt.commitment)
            {
                base.funding.push(attempt);
            }
        }
        for note in other.notes.iter() {
            if !base.notes.iter().any(|n| n.commitment == note.commitment) {
                base.notes.push(note.clone());
            }
        }
    }
    Ok(base)
}

/// Writes a loaded backup into an empty `home`, sealing the notes, wallets and submitters under
/// `passphrase`. Refuses to touch a directory that already holds state.
pub fn restore_into(
    home: &Path,
    backup: StateBackup,
    passphrase: &str,
    rounds: u32,
) -> Result<Restored, RestoreError> {
    if home.join("notes.sealed").exists() || home.join("wallets").exists() {
        return Err(RestoreError::StateExists(home.to_path_buf()));
    }
    std::fs::create_dir_all(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    }
    let notes = NoteStore::open(&home.join("notes.sealed"), passphrase, rounds)?;
    notes.save(&backup.notes)?;
    notes.restore_funding(&backup.funding)?;
    let vault = WalletVault::new(home, passphrase, rounds);
    for (key, files) in &backup.wallets {
        vault
            .import(key, files)
            .map_err(|e| RestoreError::State(e.to_string()))?;
    }
    if let Some(json) = &backup.submitters {
        SubmitterStore::open(&home.join("submitters.sealed"), passphrase, rounds)
            .and_then(|s| s.import_json(json.as_bytes()))
            .map_err(|e| RestoreError::State(e.to_string()))?;
    }
    let mut files = Vec::new();
    for (name, content) in &backup.files {
        // `config.json` is only a convenience and never replaces one that is there.
        let path = home.join(name);
        if path.exists() || name.contains('/') || name.contains("..") {
            continue;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| RestoreError::State(e.to_string()))?;
        write_private(&path, &bytes)?;
        files.push(path);
    }
    Ok(Restored {
        taken_at: backup.taken_at,
        notes: backup
            .notes
            .iter()
            .map(|n| {
                (
                    n.note_id,
                    n.status,
                    backup.wallets.contains_key(&n.commitment),
                )
            })
            .collect(),
        files,
    })
}

/// What the user is told after a restore from a backup file.
pub fn restore_notice(restored: &Restored, now_secs: u64) -> String {
    let age_days = now_secs.saturating_sub(restored.taken_at) / 86_400;
    format!(
        "Restored {} note(s) from a backup taken at Unix time {} ({} day(s) ago). Spending continues on the same notes from that state. If any lease was served after that backup was taken, the operator has already moved past it: its next answer will say so, and the only way out of an old state is an exit, which the operator can defeat. Turn backups on again (`nymirad backup-setup`) so this does not repeat.",
        restored.notes.len(),
        restored.taken_at,
        age_days
    )
}

/// What the user is told when only a note secret survives.
pub const SECRET_ONLY_NOTICE: &str = "Restored from the note secret alone: this note can be exited, not spent. The wallet state is gone, so the exit starts from the state the note had at deposit and concedes nothing. If the note was spent after the deposit, the operator can show a later state and defeat the exit, and the balance then goes to the treasury at expiry. Keep the encrypted backup on to avoid this.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::NoteStatus;

    fn note(commitment: &str, secret: &str) -> NoteEntry {
        NoteEntry {
            chain_label: "near-testnet".into(),
            secret: secret.into(),
            commitment: commitment.into(),
            denomination: "1000000".into(),
            payout: "alice.testnet".into(),
            status: NoteStatus::Accepted,
            note_id: Some(4),
            expiry_ns: Some(1_900_000_000),
            deposit_amount: Some("1000000".into()),
            tx_hash: None,
            created_at: 1,
        }
    }

    fn state() -> StateBackup {
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "0xaa".to_string(),
            Files::from([("note_state.json".to_string(), "e30=".to_string())]),
        );
        StateBackup {
            version: VERSION,
            taken_at: 1_800_000_000,
            notes: vec![note("0xaa", "cafe")],
            wallets,
            submitters: Some(r#"{"submitters":[],"assignments":{}}"#.into()),
            files: BTreeMap::new(),
            funding: vec![],
        }
    }

    #[test]
    fn a_key_backup_opens_with_the_private_key_and_nothing_else() {
        let (private, public) = generate_identity();
        let target = Target::new(parse_recipient(&public).unwrap(), vec![]);
        let file = encrypt(&target, b"state").unwrap();
        assert_eq!(&*decrypt(&private, &file).unwrap(), b"state");
        let (other, _) = generate_identity();
        assert!(decrypt(&other, &file).is_err());
        // A passphrase is not a way in to a key backup.
        assert!(decrypt("correct horse battery staple", &file).is_err());
        // It is plain age: no header.
        assert!(file.starts_with(b"age-encryption.org/v1"));
    }

    #[test]
    fn a_passphrase_backup_restores_from_the_file_and_the_passphrase_alone() {
        let pass = "correct horse battery staple";
        let (kdf, public) = passphrase_setup(pass).unwrap();
        assert_eq!(
            (kdf.alg.as_str(), kdf.m_kib, kdf.t, kdf.p),
            ("argon2id", 65_536, 3, 1)
        );
        let target = Target::new(parse_recipient(&public).unwrap(), vec![]).with_kdf(kdf);
        let file = encrypt(&target, b"state").unwrap();
        assert!(file.starts_with(HEADER.as_bytes()));
        assert_eq!(&*decrypt(pass, &file).unwrap(), b"state");
        assert!(decrypt("correct horse battery stapler", &file).is_err());
        // The salt is fresh each time, so two setups give two different keys.
        assert_ne!(public, passphrase_setup(pass).unwrap().1);
    }

    #[test]
    fn a_passphrase_under_twelve_characters_is_refused() {
        assert!(matches!(
            passphrase_setup("short pass"),
            Err(BackupError::ShortPassphrase)
        ));
        assert!(passphrase_setup("twelve chars").is_ok());
        // Characters, not bytes.
        assert!(matches!(
            passphrase_setup("éééééééééé"),
            Err(BackupError::ShortPassphrase)
        ));
    }

    #[test]
    fn every_place_is_tried_and_the_failed_one_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, b"x").unwrap();
        let (_, public) = generate_identity();
        let target = Target::new(
            parse_recipient(&public).unwrap(),
            // The first place cannot be made (a file is in the way); the second can.
            vec![blocked.join("inside"), dir.path().join("ok")],
        );
        let err = write_backups(&target, b"state", STATE_FILE).unwrap_err();
        assert!(
            matches!(&err, BackupError::Places(f) if f.len() == 1 && f[0].0 == blocked.join("inside")),
            "{err}"
        );
        assert!(dir.path().join("ok").join(STATE_FILE).exists());
    }

    #[test]
    fn a_restore_writes_sealed_state_and_never_over_existing_state() {
        let (private, public) = generate_identity();
        let place = tempfile::tempdir().unwrap();
        let target = Target::new(parse_recipient(&public).unwrap(), vec![place.path().into()]);
        write_backups(&target, &state().to_bytes().unwrap(), STATE_FILE).unwrap();

        let loaded = load(&[place.path().into()], &private).unwrap();
        let home = tempfile::tempdir().unwrap();
        let restored = restore_into(home.path(), loaded, "new-pass", 10).unwrap();
        assert_eq!(restored.notes.len(), 1);
        assert_eq!(restored.taken_at, 1_800_000_000);

        // The note comes back under the new passphrase, and the wallet is sealed with it.
        let notes = NoteStore::open(&home.path().join("notes.sealed"), "new-pass", 10).unwrap();
        assert_eq!(notes.list().unwrap()[0].secret, "cafe");
        assert!(NoteStore::open(&home.path().join("notes.sealed"), "other", 10).is_err());
        let sealed = std::fs::read(home.path().join("wallets/0xaa.sealed")).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("note_state"));

        // A second restore into the same directory is refused.
        let again = load(&[place.path().into()], &private).unwrap();
        assert!(matches!(
            restore_into(home.path(), again, "new-pass", 10),
            Err(RestoreError::StateExists(_))
        ));
    }

    #[test]
    fn the_newest_state_is_the_base_and_notes_only_files_add_the_missing_notes() {
        let (private, public) = generate_identity();
        let (old, new) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let write = |dir: &Path, st: StateBackup, name: &str| {
            let t = Target::new(parse_recipient(&public).unwrap(), vec![dir.into()]);
            write_backups(&t, &st.to_bytes().unwrap(), name).unwrap();
        };
        let mut older = state();
        older.taken_at = 10;
        older.notes[0].secret = "old".into();
        write(old.path(), older, STATE_FILE);
        write(new.path(), state(), STATE_FILE);
        // A deposit made after the last state backup is only in notes.age.
        let mut fresh = StateBackup {
            version: VERSION,
            taken_at: 1_900_000_000,
            notes: vec![note("0xbb", "beef")],
            ..Default::default()
        };
        fresh.notes[0].note_id = Some(5);
        write(new.path(), fresh, NOTES_FILE);

        let loaded = load(&[old.path().into(), new.path().into()], &private).unwrap();
        assert_eq!(loaded.taken_at, 1_800_000_000);
        let secrets: Vec<_> = loaded.notes.iter().map(|n| n.secret.as_str()).collect();
        assert_eq!(secrets, ["cafe", "beef"]);
    }

    #[test]
    fn a_missing_place_is_an_error_not_an_empty_restore() {
        let (private, _) = generate_identity();
        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            load(&[empty.path().into()], &private),
            Err(RestoreError::NoFile(_))
        ));
    }
}
