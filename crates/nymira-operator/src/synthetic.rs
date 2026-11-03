//! The daily synthetic test: open an exit at an old state on a test note and check that the
//! watcher defeats it in time, so a broken watcher shows on a quiet day and not on a real one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::alarm::Alarms;
use crate::chain::VaultView;
use crate::clock::Clock;

/// How long the watcher has to answer the test exit.
pub const ANSWER_WITHIN_SECS: u64 = 10 * 60;
const DAY_SECS: u64 = 24 * 3_600;

/// Opens the exit. The test note and the request proof that defeats the exit belong to the
/// operator's own test client; this seam keeps them out of the check.
#[async_trait]
pub trait OldStateExit: Send + Sync {
    /// Requests an exit at a state older than the one the operator holds; returns the note id.
    async fn open(&self) -> Result<u32, String>;
}

pub struct Synthetic {
    opener: Arc<dyn OldStateExit>,
    view: Arc<dyn VaultView>,
    clock: Arc<dyn Clock>,
    alarms: Arc<Alarms>,
    within_secs: u64,
    poll: Duration,
}

impl Synthetic {
    pub fn new(
        opener: Arc<dyn OldStateExit>,
        view: Arc<dyn VaultView>,
        clock: Arc<dyn Clock>,
        alarms: Arc<Alarms>,
    ) -> Self {
        Self {
            opener,
            view,
            clock,
            alarms,
            within_secs: ANSWER_WITHIN_SECS,
            poll: Duration::from_secs(5),
        }
    }

    pub fn with_timing(mut self, within_secs: u64, poll: Duration) -> Self {
        self.within_secs = within_secs;
        self.poll = poll;
        self
    }

    /// One test. True when the exit was answered in time; raises an alarm when it was not.
    pub async fn run_once(&self) -> bool {
        let started = self.clock.now_secs();
        let note_id = match self.opener.open().await {
            Ok(id) => id,
            Err(why) => {
                self.alarms
                    .raise(
                        "synthetic_open_failed",
                        "The daily watcher test could not open its exit",
                        &format!("Opening the test exit failed: {why}"),
                    )
                    .await;
                return false;
            }
        };
        self.alarms.resolve("synthetic_open_failed");
        // The test counts only an exit that was seen open and then came back to `Active`: a note
        // that never showed `PendingExit`, or that ended `Closed`, proves nothing about a defeat.
        let mut seen_pending = false;
        loop {
            if let Ok(Some(note)) = self.view.note(note_id).await {
                if note.status == "PendingExit" {
                    seen_pending = true;
                } else if seen_pending && note.status == "Active" {
                    self.alarms.resolve("synthetic_failed");
                    return true;
                } else if seen_pending {
                    self.alarms
                        .raise(
                            "synthetic_failed",
                            "The daily watcher test ended in an unexpected state",
                            &format!(
                                "Test note {note_id} left its exit as {}, not as an answered one.",
                                note.status
                            ),
                        )
                        .await;
                    return false;
                }
            }
            if self.clock.now_secs().saturating_sub(started) >= self.within_secs {
                self.alarms
                    .raise(
                        "synthetic_failed",
                        "The watcher did not answer the daily test exit",
                        &format!(
                            "Test exit on note {note_id} was still open after {} seconds. Real stale exits would not be defeated.",
                            self.within_secs
                        ),
                    )
                    .await;
                return false;
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    /// Runs the test once a day for as long as the process lives.
    pub fn spawn_daily(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.run_once().await;
                tokio::time::sleep(Duration::from_secs(DAY_SECS)).await;
            }
        })
    }
}
