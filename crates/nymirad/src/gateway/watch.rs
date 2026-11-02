//! The expiry guard and the operator's emergency notice, as the gateway runs them.
//!
//! Every background pass ([`Gateway::guard_tick`]) does four things, in this order: it reads the
//! operator's status now and then for an emergency notice; shows the expiry notices that are due;
//! tries a mutual close of each note within 5 days of expiry; and files `request_exit` for a
//! note within 3 days of it whose close did not go through. Nothing here opens a direct
//! connection: a close or exit waits while Tor is down.

use std::time::Duration;

use super::close::LeaveError;
use super::*;
use crate::expiry::{self, Due, Event, GuardConfig};

/// What the guard has tried on one note.
#[derive(Default)]
pub(super) struct Tries {
    close_failed: bool,
    last_try: u64,
}

impl Gateway {
    /// Sets the guard's switches (the default is on, closing at session end off).
    pub fn with_guard(mut self, guard: GuardConfig) -> Self {
        self.guard = guard;
        self
    }

    /// The operator's emergency notice as last read.
    pub fn emergency(&self) -> Option<String> {
        self.emergency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .clone()
    }

    /// The refusal for a new lease while an emergency notice stands.
    pub(super) fn emergency_refusal(&self) -> Option<ApiError> {
        self.emergency()
            .map(|n| unavailable("operator_emergency", self.emergency_advice(&n)))
    }

    /// What to tell the user about an emergency: leases are stopped, an exit is the way out.
    pub fn emergency_advice(&self, notice: &str) -> String {
        let ids: Vec<String> = self
            .notes
            .list()
            .unwrap_or_default()
            .iter()
            .filter(|n| n.status == NoteStatus::Accepted)
            .filter_map(|n| n.note_id.map(|id| id.to_string()))
            .collect();
        let exits = if ids.is_empty() {
            "No note is open.".to_string()
        } else {
            format!(
                "To get the money out, run `nymirad exit --note ID` for each open note (IDs: {}).",
                ids.join(", ")
            )
        };
        format!(
            "The operator reports an emergency: {notice}. No new lease is opened. Consider leaving: the operator's keys may be exposed. {exits}"
        )
    }

    /// Reads the operator's status and keeps its emergency notice. A failed read keeps what was
    /// known: an unreachable operator is not an all-clear.
    pub async fn refresh_emergency(&self) -> Option<String> {
        let now = self.clock.now_secs();
        let read = crate::status::operator_status(&self.cfg.operator_url, &self.transport).await;
        let mut known = self.emergency.lock().unwrap_or_else(|e| e.into_inner());
        known.1 = now;
        if let Ok(status) = read {
            known.0 = status["emergency_notice"].as_str().map(String::from);
        }
        known.0.clone()
    }

    /// One pass at the clock's time.
    pub async fn guard_tick(&self) -> Vec<Event> {
        self.guard_tick_at(self.clock.now_secs()).await
    }

    /// One pass as if it were `now` (Unix seconds): the expiry decisions use this time, the
    /// transactions go out for real. Tests use it to move a note's expiry closer.
    pub async fn guard_tick_at(&self, now: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let (before, read_at) = {
            let known = self.emergency.lock().unwrap_or_else(|e| e.into_inner());
            (known.0.clone(), known.1)
        };
        if read_at == 0 || self.clock.now_secs().saturating_sub(read_at) >= self.guard.status_secs {
            let after = self.refresh_emergency().await;
            if let (Some(notice), true) = (&after, after != before) {
                events.push(Event::Emergency(self.emergency_advice(notice)));
            }
        }
        if !self.guard.enabled {
            return events;
        }
        let Ok(notes) = self.notes.list() else {
            return events;
        };
        for entry in notes {
            let (Some(id), Some(expiry)) = (entry.note_id, entry.expiry_ns) else {
                continue;
            };
            let open = entry.status == NoteStatus::Accepted;
            // An exit that was started but never filed (the send failed) is finished here too.
            let stuck_exit =
                entry.status == NoteStatus::Exiting && self.exits.get(&entry.commitment).is_none();
            if !open && !stuck_exit {
                continue;
            }
            if open {
                if let Some(level) = expiry::notice_level(expiry, now) {
                    if self.notices.next(&entry.commitment, level).unwrap_or(false) {
                        events.push(Event::Notice(expiry::notice_text(
                            id,
                            level,
                            expiry::remaining(expiry, now),
                        )));
                    }
                }
            }
            match expiry::due(expiry, now) {
                Due::Nothing => {}
                Due::Close if open => self.leave(&entry, Due::Close, now, &mut events).await,
                Due::Exit => self.leave(&entry, Due::Exit, now, &mut events).await,
                Due::Close => {}
            }
        }
        events
    }

    /// Closes the note, or exits it when the close has failed and the time is short.
    async fn leave(&self, entry: &NoteEntry, due: Due, now: u64, events: &mut Vec<Event>) {
        let id = entry.note_id.unwrap_or(0);
        let key = &entry.commitment;
        let close_failed = {
            let tries = self.guard_tries.lock().unwrap_or_else(|e| e.into_inner());
            match tries.get(key) {
                Some(t)
                    if t.last_try != 0
                        && now.saturating_sub(t.last_try) < self.guard.retry_secs =>
                {
                    return;
                }
                Some(t) => t.close_failed,
                None => false,
            }
        };
        // A lease on the note blocks both: it is told to retire and the next pass goes on.
        if self.sessions.lock().await.contains_key(key) {
            if self.retire(key).await.is_ok() {
                events.push(Event::Retiring(id));
            }
            return;
        }
        let mut failed = close_failed;
        let wants_close = entry.status == NoteStatus::Accepted && !(due == Due::Exit && failed);
        if wants_close {
            match self.close_note(id, self.guard.close.clone(), None).await {
                Ok(c) if c.confirmed => {
                    events.push(Event::Closed(id));
                    return;
                }
                Ok(_) => {
                    events.push(Event::CloseFailed(
                        id,
                        "sent, but the vault does not show the note closed yet".into(),
                    ));
                    self.mark(key, failed, now);
                    return;
                }
                Err(LeaveError::Busy(_) | LeaveError::NoNote(_)) => return,
                Err(e) => {
                    failed = true;
                    events.push(Event::CloseFailed(id, e.to_string()));
                }
            }
        }
        if due == Due::Exit && (failed || entry.status == NoteStatus::Exiting) {
            match self.exit_note(id, None, None).await {
                Ok(_) => events.push(Event::ExitFiled(id)),
                Err(LeaveError::Busy(_) | LeaveError::NoNote(_)) => return,
                Err(e) => events.push(Event::ExitFailed(id, e.to_string())),
            }
        }
        self.mark(key, failed, now);
    }

    fn mark(&self, key: &str, close_failed: bool, now: u64) {
        self.guard_tries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.to_string(),
                Tries {
                    close_failed,
                    last_try: now,
                },
            );
    }

    /// The end of a session (the daemon stops). With `close_at_session_end` on, every open note
    /// is closed once its lease has settled; off, nothing is closed. Leases are retired either
    /// way. Waits up to `end_wait_secs` for the operator to settle them.
    pub async fn end_session(&self) -> Vec<Event> {
        self.reap(true).await;
        if !self.guard.close_at_session_end {
            return Vec::new();
        }
        let waits = self.guard.end_wait_secs / 2;
        for _ in 0..=waits {
            self.settle().await;
            if self.sessions.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let mut events = Vec::new();
        for entry in self.notes.list().unwrap_or_default() {
            let (Some(id), NoteStatus::Accepted) = (entry.note_id, entry.status) else {
                continue;
            };
            match self.close_note(id, self.guard.close.clone(), None).await {
                Ok(c) if c.confirmed => events.push(Event::Closed(id)),
                Ok(_) => events.push(Event::CloseFailed(id, "not confirmed yet".into())),
                Err(e) => events.push(Event::CloseFailed(id, e.to_string())),
            }
        }
        events
    }
}
