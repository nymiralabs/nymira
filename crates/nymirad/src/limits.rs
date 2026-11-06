//! The counters behind an agent's spending limits: the rate window, the day's total and what is
//! reserved against it.
//!
//! Today's total is what settled leases charged today (UTC), plus the bound of every open lease,
//! plus the worst case of every call in flight. Settlement replaces the bound and the worst
//! cases with the charge. The settled part is kept in `counters-<agent>.json` in the owner's
//! state directory (0600, in a directory the agent's OS user cannot write); the rest lives in
//! memory and is rebuilt as leases open. After a restart or a restore the settled part is never
//! lower than the charges the lease records show for today.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::records::LeaseRecord;

const DAY_SECS: u64 = 86_400;
const MINUTE_SECS: u64 = 60;

/// The file in the state directory that holds an agent's settled total for the day.
pub fn file_name(agent: &str) -> String {
    format!("counters-{agent}.json")
}

/// Whole micro-USD of a policy amount.
pub fn micro_of(usd: f64) -> u128 {
    (usd * 1e6 + 1e-6).floor() as u128
}

/// The sum of the charges of leases that closed on `day` (days since the Unix epoch) on the
/// note `note_id`: the least today's total can be.
pub fn charges_on(records: &[LeaseRecord], note_id: u32, day: u64) -> u128 {
    records
        .iter()
        .filter(|r| r.note_id == note_id && r.closed_at.is_some_and(|t| t / DAY_SECS == day))
        .filter_map(|r| r.charge.as_deref()?.parse::<u128>().ok())
        .sum()
}

#[derive(Serialize, Deserialize)]
struct Saved {
    day: u64,
    charged_micro: String,
}

#[derive(Default)]
struct Agent {
    day: u64,
    charged: u128,
    leases: u128,
    inflight: u128,
    window: VecDeque<u64>,
}

impl Agent {
    fn total(&self) -> u128 {
        self.charged + self.leases + self.inflight
    }
}

type Table = HashMap<String, Agent>;

/// All agents' counters. Clones share the same state.
#[derive(Clone)]
pub struct Limits {
    home: PathBuf,
    table: Arc<Mutex<Table>>,
}

/// A call's worst case, counted against the day while the call is in flight.
pub struct CallHold {
    table: Arc<Mutex<Table>>,
    agent: String,
    micro: u128,
}

impl Drop for CallHold {
    fn drop(&mut self) {
        let mut table = lock(&self.table);
        if let Some(a) = table.get_mut(&self.agent) {
            a.inflight = a.inflight.saturating_sub(self.micro);
        }
    }
}

fn lock(table: &Mutex<Table>) -> MutexGuard<'_, Table> {
    table.lock().unwrap_or_else(|e| e.into_inner())
}

impl Limits {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            table: Default::default(),
        }
    }

    /// The agent's counters for `now`: loaded on first use (the saved total, raised to `floor`),
    /// and reset when the UTC day has changed.
    fn agent<'a>(
        &self,
        table: &'a mut Table,
        name: &str,
        now: u64,
        floor: &dyn Fn() -> u128,
    ) -> &'a mut Agent {
        let day = now / DAY_SECS;
        let a = table.entry(name.to_string()).or_insert_with(|| {
            let saved = std::fs::read(self.home.join(file_name(name)))
                .ok()
                .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
                .filter(|s| s.day == day)
                .and_then(|s| s.charged_micro.parse::<u128>().ok())
                .unwrap_or(0);
            Agent {
                day,
                charged: saved.max(floor()),
                ..Default::default()
            }
        });
        if a.day != day {
            a.day = day;
            a.charged = 0;
        }
        a
    }

    /// Counts one request in the last minute. `Err` carries the seconds until a slot frees up.
    pub fn rate(
        &self,
        name: &str,
        now: u64,
        per_min: u32,
        floor: &dyn Fn() -> u128,
    ) -> Result<(), u64> {
        self.rate_at(name, now, per_min, floor, true)
    }

    /// The same answer as `rate`, with no slot taken (the quote route).
    pub fn peek_rate(
        &self,
        name: &str,
        now: u64,
        per_min: u32,
        floor: &dyn Fn() -> u128,
    ) -> Result<(), u64> {
        self.rate_at(name, now, per_min, floor, false)
    }

    fn rate_at(
        &self,
        name: &str,
        now: u64,
        per_min: u32,
        floor: &dyn Fn() -> u128,
        take: bool,
    ) -> Result<(), u64> {
        let mut table = lock(&self.table);
        let a = self.agent(&mut table, name, now, floor);
        while a.window.front().is_some_and(|t| t + MINUTE_SECS <= now) {
            a.window.pop_front();
        }
        if a.window.len() >= per_min as usize {
            let oldest = a.window.front().copied().unwrap_or(now);
            return Err((oldest + MINUTE_SECS).saturating_sub(now).max(1));
        }
        if take {
            a.window.push_back(now);
        }
        Ok(())
    }

    /// Whether `reserve_call` would succeed now, with nothing reserved.
    pub fn has_room(
        &self,
        name: &str,
        now: u64,
        call_micro: u128,
        lease_bound_micro: u128,
        max_daily_micro: u128,
        floor: &dyn Fn() -> u128,
    ) -> bool {
        let mut table = lock(&self.table);
        let a = self.agent(&mut table, name, now, floor);
        let lease = if a.leases == 0 { lease_bound_micro } else { 0 };
        a.total() + call_micro + lease <= max_daily_micro
    }

    /// Reserves a call's worst case against the day. A call that opens the agent's first lease
    /// also carries that lease's bound. Checked and taken under one lock, so calls that arrive
    /// together cannot each find room that only one of them can have.
    pub fn reserve_call(
        &self,
        name: &str,
        now: u64,
        call_micro: u128,
        lease_bound_micro: u128,
        max_daily_micro: u128,
        floor: &dyn Fn() -> u128,
    ) -> Option<CallHold> {
        let mut table = lock(&self.table);
        let a = self.agent(&mut table, name, now, floor);
        let lease = if a.leases == 0 { lease_bound_micro } else { 0 };
        if a.total() + call_micro + lease > max_daily_micro {
            return None;
        }
        a.inflight += call_micro;
        Some(CallHold {
            table: self.table.clone(),
            agent: name.to_string(),
            micro: call_micro,
        })
    }

    /// A lease opened on the agent's note: its bound counts until it settles.
    pub fn lease_opened(&self, name: &str, now: u64, bound_micro: u128, floor: &dyn Fn() -> u128) {
        let mut table = lock(&self.table);
        self.agent(&mut table, name, now, floor).leases += bound_micro;
    }

    /// A lease settled: the charge replaces its bound, and the total is saved.
    pub fn settled(
        &self,
        name: &str,
        now: u64,
        bound_micro: u128,
        charge_micro: u128,
        floor: &dyn Fn() -> u128,
    ) {
        let mut table = lock(&self.table);
        let a = self.agent(&mut table, name, now, floor);
        a.leases = a.leases.saturating_sub(bound_micro);
        a.charged += charge_micro;
        if let Err(e) = self.save(name, a) {
            eprintln!("warning: the daily counter of agent {name} was not saved: {e}");
        }
    }

    /// What settled leases charged today.
    pub fn charged_today(&self, name: &str, now: u64, floor: &dyn Fn() -> u128) -> u128 {
        let mut table = lock(&self.table);
        self.agent(&mut table, name, now, floor).charged
    }

    fn save(&self, name: &str, a: &Agent) -> std::io::Result<()> {
        let path = self.home.join(file_name(name));
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        let saved = Saved {
            day: a.day,
            charged_micro: a.charged.to_string(),
        };
        file.write_all(&serde_json::to_vec(&saved).expect("counter serializes"))?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 20_000 * DAY_SECS;
    const NO_FLOOR: &dyn Fn() -> u128 = &|| 0;

    fn limits() -> (tempfile::TempDir, Limits) {
        let dir = tempfile::tempdir().unwrap();
        let l = Limits::new(dir.path());
        (dir, l)
    }

    fn record(note_id: u32, closed_at: Option<u64>, charge: Option<&str>) -> LeaseRecord {
        serde_json::from_value(serde_json::json!({
            "lease_id": "l", "note_id": note_id, "root": "0x", "leaf_count": 1,
            "snapshot_block": 1, "bound_class": "100000", "opened_at": 1,
            "closed_at": closed_at, "charge": charge, "expected_charge": null, "model": "m",
            "sampling": {}, "requests": 1, "message_count": 1, "message_bytes": 1,
            "prompt_tokens": 1, "completion_tokens": 1, "trust": null, "warnings": [],
        }))
        .unwrap()
    }

    #[test]
    fn the_rate_window_frees_a_slot_after_a_minute() {
        let (_d, l) = limits();
        for i in 0..3 {
            l.rate("a", DAY + i, 3, NO_FLOOR).unwrap();
        }
        assert_eq!(l.rate("a", DAY + 10, 3, NO_FLOOR), Err(50));
        assert_eq!(l.rate("a", DAY + 59, 3, NO_FLOOR), Err(1));
        l.rate("a", DAY + 60, 3, NO_FLOOR).unwrap();
        // Another agent has its own window.
        l.rate("b", DAY + 10, 3, NO_FLOOR).unwrap();
    }

    #[test]
    fn calls_in_flight_and_the_open_lease_count_and_a_dropped_hold_frees_its_share() {
        let (_d, l) = limits();
        // 100_000 bound + 50_000 call = 150_000 fits a day of 170_000; a second call does not.
        let first = l
            .reserve_call("a", DAY, 50_000, 100_000, 170_000, NO_FLOOR)
            .unwrap();
        assert!(l
            .reserve_call("a", DAY, 50_000, 100_000, 170_000, NO_FLOOR)
            .is_none());
        drop(first);
        // The lease is open now: its bound is in the total and the next call does not add it twice.
        l.lease_opened("a", DAY, 100_000, NO_FLOOR);
        let _again = l
            .reserve_call("a", DAY, 50_000, 100_000, 170_000, NO_FLOOR)
            .unwrap();
        assert!(l
            .reserve_call("a", DAY, 50_000, 100_000, 170_000, NO_FLOOR)
            .is_none());
    }

    #[test]
    fn settlement_replaces_the_bound_with_the_charge_and_the_day_resets_at_midnight_utc() {
        let (dir, l) = limits();
        l.lease_opened("a", DAY, 100_000, NO_FLOOR);
        l.settled("a", DAY + 5, 100_000, 2_000, NO_FLOOR);
        assert_eq!(l.charged_today("a", DAY + 6, NO_FLOOR), 2_000);
        assert!(l
            .reserve_call("a", DAY + 6, 1, 100_000, 2_000, NO_FLOOR)
            .is_none());
        assert!(l
            .reserve_call("a", DAY + 6, 1, 100_000, 102_001, NO_FLOOR)
            .is_some());
        // The saved total survives a restart.
        let restarted = Limits::new(dir.path());
        assert_eq!(restarted.charged_today("a", DAY + 7, NO_FLOOR), 2_000);
        // The next UTC day starts at zero, and so does a total saved on the day before.
        assert_eq!(l.charged_today("a", DAY + DAY_SECS, NO_FLOOR), 0);
        assert_eq!(restarted.charged_today("a", DAY + DAY_SECS, NO_FLOOR), 0);
        let fresh = Limits::new(dir.path());
        assert_eq!(fresh.charged_today("a", DAY + DAY_SECS, NO_FLOOR), 0);
    }

    #[test]
    fn the_saved_total_is_private_and_a_restart_never_goes_below_the_lease_records() {
        let (dir, l) = limits();
        l.settled("a", DAY, 0, 4_000, NO_FLOOR);
        let path = dir.path().join(file_name("a"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A restore brings back an older counter (or none) next to newer lease records.
        std::fs::write(&path, br#"{"day":20000,"charged_micro":"1000"}"#).unwrap();
        let records = [
            record(7, Some(DAY + 10), Some("3000")),
            record(7, Some(DAY + 20), Some("4000")),
            record(7, Some(DAY - 5), Some("9000")),
            record(8, Some(DAY + 20), Some("9000")),
            record(7, None, None),
        ];
        let floor = || charges_on(&records, 7, 20_000);
        assert_eq!(floor(), 7_000);
        let restored = Limits::new(dir.path());
        assert_eq!(restored.charged_today("a", DAY + 30, &floor), 7_000);
        std::fs::remove_file(&path).unwrap();
        let gone = Limits::new(dir.path());
        assert_eq!(gone.charged_today("a", DAY + 30, &floor), 7_000);
        // A counter above the records stays.
        std::fs::write(&path, br#"{"day":20000,"charged_micro":"8000"}"#).unwrap();
        assert_eq!(
            Limits::new(dir.path()).charged_today("a", DAY + 30, &floor),
            8_000
        );
    }

    #[test]
    fn policy_amounts_become_whole_micro_usd() {
        assert_eq!(micro_of(0.05), 50_000);
        assert_eq!(micro_of(0.1), 100_000);
        assert_eq!(micro_of(2.0), 2_000_000);
        assert_eq!(micro_of(0.000001), 1);
    }
}
