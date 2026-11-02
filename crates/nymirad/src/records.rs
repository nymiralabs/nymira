//! Lease records: metadata of each lease, never message content. Kept as an append-only file in
//! the state directory, last line per lease wins, and carried by the backup.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::attest::Trust;

pub const FILE: &str = "lease-records.jsonl";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub note_id: u32,
    /// The root the proof used, the leaves under it and the final block they were read at.
    pub root: String,
    pub leaf_count: u32,
    pub snapshot_block: u64,
    /// Micro-USD, as a string.
    pub bound_class: String,
    pub opened_at: u64,
    pub closed_at: Option<u64>,
    pub charge: Option<String>,
    pub expected_charge: Option<String>,
    pub model: String,
    /// `temperature`, `top_p`, `seed` and the like, as sent.
    pub sampling: Value,
    pub requests: u32,
    pub message_count: u32,
    pub message_bytes: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub trust: Option<Trust>,
    pub warnings: Vec<String>,
}

pub struct Records {
    path: PathBuf,
}

impl Records {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the record's current state; the file is synced before this returns.
    pub fn write(&self, record: &LeaseRecord) -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&self.path)?;
        let mut line = serde_json::to_vec(record).expect("record serializes");
        line.push(b'\n');
        file.write_all(&line)?;
        file.sync_all()
    }

    /// Every lease, in the order first written, with its latest state.
    pub fn list(&self) -> Vec<LeaseRecord> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut out: Vec<LeaseRecord> = Vec::new();
        for record in text
            .lines()
            .filter_map(|l| serde_json::from_str::<LeaseRecord>(l).ok())
        {
            match out.iter_mut().find(|r| r.lease_id == record.lease_id) {
                Some(slot) => *slot = record,
                None => out.push(record),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> LeaseRecord {
        LeaseRecord {
            lease_id: id.into(),
            note_id: 0,
            root: "0x01".into(),
            leaf_count: 1,
            snapshot_block: 10,
            bound_class: "100000".into(),
            opened_at: 5,
            closed_at: None,
            charge: None,
            expected_charge: None,
            model: "m".into(),
            sampling: serde_json::json!({"temperature": 0.2}),
            requests: 0,
            message_count: 0,
            message_bytes: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            trust: None,
            warnings: vec![],
        }
    }

    #[test]
    fn the_last_state_of_a_lease_wins_and_the_file_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = Records::new(dir.path());
        store.write(&record("a")).unwrap();
        store.write(&record("b")).unwrap();
        let mut done = record("a");
        done.charge = Some("42000".into());
        done.trust = Some(Trust::ModelSigned);
        store.write(&done).unwrap();
        let all = store.list();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].charge.as_deref(), Some("42000"));
        assert_eq!(all[0].trust, Some(Trust::ModelSigned));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
