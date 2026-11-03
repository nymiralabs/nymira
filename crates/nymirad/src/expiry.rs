//! The expiry guard: what `nymirad` does about a note that is running out of time.
//!
//! A note that reaches its expiry goes to the treasury. So, counting back from the expiry:
//!
//! - 14, 7 and 2 days: a notice (once per level per note);
//! - 5 days: no new lease on the note, and a mutual close is tried;
//! - 3 days: if the close did not go through, `request_exit` is filed.
//!
//! The guard is on by default and can be switched off. Closing at the end of a session is a
//! separate switch and is off by default: every close shows the note ID and the total spend.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::gateway::close::CloseOptions;

pub const DAY: u64 = 86_400;
/// No new lease when this many days or fewer are left; a mutual close is tried.
pub const CLOSE_DAYS: u64 = 5;
/// `request_exit` is filed when this many days or fewer are left and the note is still open.
pub const EXIT_DAYS: u64 = 3;
/// Notice levels, in days before expiry.
pub const NOTICE_DAYS: [u64; 3] = [14, 7, 2];
pub const NOTICES_FILE: &str = "expiry-notices.json";

#[derive(Clone, Debug)]
pub struct GuardConfig {
    /// The whole guard: the lease cut-off, the automatic close and exit, the notices.
    pub enabled: bool,
    /// Close every open note when the daemon stops. Off by default.
    pub close_at_session_end: bool,
    /// How a guard close goes out (the delay, the submitter family, polling).
    pub close: CloseOptions,
    /// A failed close or exit is tried again after this many seconds.
    pub retry_secs: u64,
    /// The operator's status is read for an emergency notice at most this often.
    pub status_secs: u64,
    /// At session end, how long to wait for open leases to settle before closing.
    pub end_wait_secs: u64,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            close_at_session_end: false,
            close: CloseOptions::default(),
            retry_secs: 3_600,
            status_secs: 300,
            end_wait_secs: 120,
        }
    }
}

/// Seconds left until `expiry_secs`.
pub fn remaining(expiry_secs: u64, now_secs: u64) -> u64 {
    expiry_secs.saturating_sub(now_secs)
}

/// No new lease: the note is within [`CLOSE_DAYS`] of its expiry (or past it).
pub fn lease_blocked(expiry_secs: u64, now_secs: u64) -> bool {
    remaining(expiry_secs, now_secs) <= CLOSE_DAYS * DAY
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    Nothing,
    /// Try a mutual close.
    Close,
    /// File `request_exit` if the close did not go through.
    Exit,
}

pub fn due(expiry_secs: u64, now_secs: u64) -> Due {
    let left = remaining(expiry_secs, now_secs);
    if left <= EXIT_DAYS * DAY {
        Due::Exit
    } else if left <= CLOSE_DAYS * DAY {
        Due::Close
    } else {
        Due::Nothing
    }
}

/// The notice level (14, 7 or 2) that applies with this much time left: the smallest one not
/// yet passed. `None` when more than 14 days are left.
pub fn notice_level(expiry_secs: u64, now_secs: u64) -> Option<u64> {
    let left = remaining(expiry_secs, now_secs);
    NOTICE_DAYS
        .iter()
        .copied()
        .filter(|d| left <= d * DAY)
        .min()
}

pub fn notice_text(note_id: u32, level: u64, left_secs: u64) -> String {
    let left = if left_secs >= DAY {
        format!("{} days", left_secs / DAY)
    } else {
        "less than a day".to_string()
    };
    format!(
        "note {note_id} expires in {left} ({level}-day notice). At 5 days nymirad stops opening leases on it and tries to close it; at 3 days it files an exit. To act now: `nymirad close --note {note_id}`."
    )
}

/// Which notice level was last shown for each note, so a restart does not repeat it. Public
/// values only; the file is mode 0600.
pub struct NoticeBook {
    path: PathBuf,
}

impl NoticeBook {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(NOTICES_FILE),
        }
    }

    fn load(&self) -> BTreeMap<String, u64> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The level to show now for `commitment`, if it is lower than the last one shown. Records
    /// it before returning.
    pub fn next(&self, commitment: &str, level: u64) -> std::io::Result<bool> {
        let mut all = self.load();
        if all.get(commitment).is_some_and(|shown| *shown <= level) {
            return Ok(false);
        }
        all.insert(commitment.into(), level);
        let tmp = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&all).expect("notices serialize"))?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(true)
    }
}

/// What the guard did in one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Notice(String),
    /// A lease on the note was told to retire so the close can start.
    Retiring(u32),
    Closed(u32),
    CloseFailed(u32, String),
    ExitFiled(u32),
    ExitFailed(u32, String),
    /// The operator's emergency notice, with what to do.
    Emergency(String),
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Notice(t) => write!(f, "notice: {t}"),
            Event::Retiring(n) => write!(f, "note {n}: retiring its lease before the close"),
            Event::Closed(n) => write!(f, "note {n}: closed before it expires"),
            Event::CloseFailed(n, why) => write!(f, "warning: note {n}: close failed: {why}"),
            Event::ExitFiled(n) => write!(
                f,
                "note {n}: the close did not go through; request_exit filed. finalize_exit follows after the exit window"
            ),
            Event::ExitFailed(n, why) => write!(
                f,
                "warning: note {n}: request_exit failed: {why}. Fix it before the note expires or its balance goes to the treasury"
            ),
            Event::Emergency(t) => write!(f, "warning: {t}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn the_cut_offs_are_five_and_three_days() {
        let at = |days_left_secs: u64| NOW + days_left_secs;
        assert!(!lease_blocked(at(5 * DAY + 1), NOW));
        assert!(lease_blocked(at(5 * DAY), NOW));
        assert_eq!(due(at(5 * DAY + 1), NOW), Due::Nothing);
        assert_eq!(due(at(5 * DAY), NOW), Due::Close);
        assert_eq!(due(at(3 * DAY + 1), NOW), Due::Close);
        assert_eq!(due(at(3 * DAY), NOW), Due::Exit);
        assert_eq!(due(NOW - 10, NOW), Due::Exit);
        assert!(lease_blocked(NOW - 10, NOW));
    }

    #[test]
    fn notices_come_at_14_7_and_2_days() {
        let level = |secs: u64| notice_level(NOW + secs, NOW);
        assert_eq!(level(14 * DAY + 1), None);
        assert_eq!(level(14 * DAY), Some(14));
        assert_eq!(level(8 * DAY), Some(14));
        assert_eq!(level(7 * DAY), Some(7));
        assert_eq!(level(2 * DAY + 1), Some(7));
        assert_eq!(level(2 * DAY), Some(2));
        assert_eq!(level(0), Some(2));
    }

    #[test]
    fn a_level_is_shown_once_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let book = NoticeBook::new(dir.path());
        assert!(book.next("0xaa", 14).unwrap());
        assert!(!book.next("0xaa", 14).unwrap());
        // A new process reads the same file.
        let book = NoticeBook::new(dir.path());
        assert!(!book.next("0xaa", 14).unwrap());
        assert!(book.next("0xaa", 7).unwrap());
        assert!(!book.next("0xaa", 14).unwrap());
        assert!(book.next("0xbb", 7).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(NOTICES_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
