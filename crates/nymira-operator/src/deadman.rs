//! The dead-man switch: a separate process that reads `/v1/status` from outside and alerts a
//! person when the operator or one of its watchers has gone quiet.
//!
//! It shares nothing with the operator but the public status document, so a stopped watcher, a
//! stopped operator and a dead heartbeat all show up here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::alarm::Alarms;
use crate::clock::Clock;

/// A watcher silent this long, or a status that stays unreachable this long, raises an alarm.
pub const SILENT_SECS: u64 = 30 * 60;
/// No heartbeat for this long warns a person (`/v1/status` says `degraded` at 2 hours).
pub const HEARTBEAT_WARN_SECS: u64 = 4 * 3_600;
const CHECK_EVERY_SECS: u64 = 60;

#[derive(Default)]
struct Seen {
    /// First time the status was read; a watcher that never polled counts from here.
    first: Option<u64>,
    /// Since when each watcher has shown no poll time (after an operator restart, say).
    unpolled: std::collections::HashMap<String, u64>,
    /// Since when the status has been unreachable.
    down_since: Option<u64>,
}

pub struct Deadman {
    status_url: String,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    alarms: Arc<Alarms>,
    seen: Mutex<Seen>,
}

impl Deadman {
    /// `status_url` is the full URL of `/v1/status`.
    pub fn new(status_url: impl Into<String>, clock: Arc<dyn Clock>, alarms: Arc<Alarms>) -> Self {
        Self {
            status_url: status_url.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            clock,
            alarms,
            seen: Mutex::new(Seen::default()),
        }
    }

    async fn fetch(&self) -> Option<Value> {
        let reply = self.http.get(&self.status_url).send().await.ok()?;
        if !reply.status().is_success() {
            return None;
        }
        reply.json().await.ok()
    }

    /// One check.
    pub async fn tick(&self) {
        let now = self.clock.now_secs();
        let Some(status) = self.fetch().await else {
            let since = *self
                .seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .down_since
                .get_or_insert(now);
            if now.saturating_sub(since) >= SILENT_SECS {
                self.alarms
                    .raise(
                        "operator_unreachable",
                        "The operator status is unreachable",
                        &format!(
                            "/v1/status has not answered for {} minutes.",
                            (now - since) / 60
                        ),
                    )
                    .await;
            }
            return;
        };
        let first = {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            seen.down_since = None;
            *seen.first.get_or_insert(now)
        };
        self.alarms.resolve("operator_unreachable");

        for w in status["watchers"].as_array().into_iter().flatten() {
            let id = w["id"].as_str().unwrap_or("?").to_string();
            // A watcher with no poll time counts from when we first saw it without one.
            let last = match w["last_poll_ts"].as_u64() {
                Some(ts) => {
                    self.seen
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .unpolled
                        .remove(&id);
                    ts
                }
                None => *self
                    .seen
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .unpolled
                    .entry(id.clone())
                    .or_insert(now),
            };
            let key = format!("watcher_silent:{id}");
            if now.saturating_sub(last) >= SILENT_SECS {
                self.alarms
                    .raise(
                        &key,
                        "A watcher is silent",
                        &format!(
                            "Watcher {id} has not completed a poll for {} minutes.",
                            (now - last) / 60
                        ),
                    )
                    .await;
            } else {
                self.alarms.resolve(&key);
            }
        }

        let age = status["heartbeat_age_secs"]
            .as_u64()
            .unwrap_or_else(|| now.saturating_sub(first));
        if age >= HEARTBEAT_WARN_SECS {
            self.alarms
                .raise(
                    "heartbeat_silent",
                    "No heartbeat for 4 hours",
                    &format!("The last heartbeat is {} minutes old.", age / 60),
                )
                .await;
        } else {
            self.alarms.resolve("heartbeat_silent");
        }
    }

    /// Checks once a minute for as long as the process lives.
    pub fn spawn(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.tick().await;
                tokio::time::sleep(Duration::from_secs(CHECK_EVERY_SECS)).await;
            }
        })
    }
}
