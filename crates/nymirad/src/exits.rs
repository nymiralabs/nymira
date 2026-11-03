//! What `nymirad` remembers about notes that left: the exit deadline, so `finalize_exit` can be
//! sent without reading the chain, and the spend at the time, for the direct-send confirmation.
//! Public values only (a note ID, a time, a sum); the file carries no secret and is mode 0600.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "exits.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    pub note_id: u32,
    /// Unix seconds after which `finalize_exit` works; `None` for a note that was closed.
    pub deadline_secs: Option<u64>,
    /// What the note had spent when it left, in token base units.
    pub spent: String,
}

pub struct ExitBook {
    path: PathBuf,
}

impl ExitBook {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(FILE),
        }
    }

    fn load(&self) -> BTreeMap<String, Left> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, all: &BTreeMap<String, Left>) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(all).expect("exits serialize"))?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Records (or replaces) the entry of the note with `commitment`.
    pub fn put(&self, commitment: &str, left: Left) -> std::io::Result<()> {
        let mut all = self.load();
        all.insert(commitment.into(), left);
        self.save(&all)
    }

    pub fn get(&self, commitment: &str) -> Option<Left> {
        self.load().remove(commitment)
    }

    /// Entries with a deadline, as `(commitment, entry)`.
    pub fn pending(&self) -> Vec<(String, Left)> {
        self.load()
            .into_iter()
            .filter(|(_, l)| l.deadline_secs.is_some())
            .collect()
    }

    /// Clears the deadline of a note whose exit was paid out.
    pub fn finalized(&self, commitment: &str) -> std::io::Result<()> {
        let mut all = self.load();
        if let Some(l) = all.get_mut(commitment) {
            l.deadline_secs = None;
        }
        self.save(&all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_is_pending_until_the_exit_is_paid() {
        let dir = tempfile::tempdir().unwrap();
        let book = ExitBook::new(dir.path());
        assert!(book.pending().is_empty());
        book.put(
            "0xaa",
            Left {
                note_id: 3,
                deadline_secs: Some(100),
                spent: "5".into(),
            },
        )
        .unwrap();
        assert_eq!(book.pending().len(), 1);
        book.finalized("0xaa").unwrap();
        assert!(book.pending().is_empty());
        assert_eq!(book.get("0xaa").unwrap().spent, "5");
    }
}
