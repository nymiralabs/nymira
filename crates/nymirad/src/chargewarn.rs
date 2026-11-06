//! Charge warnings on budget notes.
//!
//! When the operator charges a budget note's lease more than one charge step above what the
//! signed usage and the vault's rules give, the note is marked here. No new agent request is
//! served on a marked note (503 `unavailable`) until the owner acknowledges the warning with
//! `nymirad agent ack-warning`. The marks are a list of note commitments in the state
//! directory (0600), read on every request so an acknowledgement from the CLI applies at once.

use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "charge-warnings.json";

#[derive(Clone, Debug)]
pub struct ChargeWarnings {
    path: PathBuf,
}

impl ChargeWarnings {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(FILE),
        }
    }

    fn load(&self) -> Vec<String> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn store(&self, notes: &[String]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(notes).expect("a list serializes"))?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Whether a warning on `note` has not been acknowledged.
    pub fn active(&self, note: &str) -> bool {
        self.load().iter().any(|n| n == note)
    }

    pub fn raise(&self, note: &str) -> std::io::Result<()> {
        let mut all = self.load();
        if !all.iter().any(|n| n == note) {
            all.push(note.into());
            self.store(&all)?;
        }
        Ok(())
    }

    /// Clears the warning; false when there was none.
    pub fn ack(&self, note: &str) -> std::io::Result<bool> {
        let mut all = self.load();
        let before = all.len();
        all.retain(|n| n != note);
        let had = all.len() != before;
        if had {
            self.store(&all)?;
        }
        Ok(had)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warning_stays_until_it_is_acknowledged_and_belongs_to_one_note() {
        let dir = tempfile::tempdir().unwrap();
        let w = ChargeWarnings::new(dir.path());
        assert!(!w.active("0xa"));
        w.raise("0xa").unwrap();
        w.raise("0xa").unwrap();
        assert!(w.active("0xa") && !w.active("0xb"));
        assert!(ChargeWarnings::new(dir.path()).active("0xa"));
        assert!(w.ack("0xa").unwrap());
        assert!(!w.ack("0xa").unwrap());
        assert!(!w.active("0xa"));
    }
}
