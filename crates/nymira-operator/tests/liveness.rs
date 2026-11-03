//! Alarms and liveness (#88): heartbeat age, watcher poll times, the dead-man switch, provider
//! disagreement, exit waves and the daily synthetic test.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::{ExitEntry, FakeVault, NoteRecord, VaultView};
use nymira_operator::clock::Clock;
use nymira_operator::deadman::Deadman;
use nymira_operator::store;
use nymira_operator::synthetic::{OldStateExit, Synthetic};
use nymira_operator::watcher::{FakeCalls, Watcher, WatcherConfig};

struct World {
    rig: Rig,
    second: Arc<FakeVault>,
    watcher: Arc<Watcher>,
    alarms: Arc<Alarms>,
}

async fn world() -> World {
    let rig = Rig::new(Setup::default()).await;
    let second = Arc::new(FakeVault::new(vault_config(), &root_hex()));
    let alarms = Arc::new(Alarms::new(vec![rig.sink.clone()], rig.clock.clone()));
    let providers: Vec<Arc<dyn VaultView>> = vec![rig.vault.clone(), second.clone()];
    let watcher = Arc::new(
        Watcher::new(
            providers,
            Arc::new(FakeCalls::default()),
            rig.op.store().clone(),
            rig.clock.clone(),
            alarms.clone(),
            WatcherConfig::default(),
        )
        .unwrap(),
    );
    rig.op.register_watcher("laptop", watcher.clone());
    World {
        rig,
        second,
        watcher,
        alarms,
    }
}

fn exit(note_id: u32, n: u64) -> ExitEntry {
    ExitEntry {
        note_id,
        nullifier: nullifier_hex(n),
        next_nullifier: nullifier_hex(n + 100),
        final_balance: 1_990_000,
        conceded: 0,
        deadline: (T0 as u128 + 7 * 86_400) * 1_000_000_000,
    }
}

fn record(id: u32) -> NoteRecord {
    NoteRecord {
        note_id: id,
        deposit_amount: 2_000_000,
        expiry: EXPIRY,
        status: "PendingExit".into(),
        leaf_retired: false,
    }
}

impl World {
    fn both(&self, f: impl Fn(&mut nymira_operator::chain::FakeVaultState)) {
        self.rig.vault.edit(&f);
        self.second.edit(&f);
    }

    async fn heartbeat_at(&self, at: u64) {
        let store = self.rig.op.store().clone();
        store
            .run(move |c| {
                let id =
                    store::insert_tx_intent(c, "heartbeat", "heartbeat", b"{}", 1, "k", None, at)?;
                store::finish_tx(c, id, "final", "")
            })
            .await
            .unwrap();
    }

    fn alarm_keys(&self) -> Vec<String> {
        self.rig.sink.sent().into_iter().map(|a| a.key).collect()
    }

    fn deadman(&self) -> Deadman {
        Deadman::new(
            format!("{}/v1/status", self.rig.url),
            self.rig.clock.clone(),
            self.alarms.clone(),
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn status_shows_heartbeat_age_watcher_polls_and_degrades_after_two_hours() {
    let w = world().await;
    w.heartbeat_at(T0).await;
    w.watcher.poll().await;
    w.rig.clock.advance_secs(600);
    let s = w.rig.status().await;
    assert_eq!(s["heartbeat_age_secs"], 600);
    assert_eq!(s["degraded"], false);
    assert_eq!(s["watchers"][0]["id"], "laptop");
    assert_eq!(s["watchers"][0]["last_poll_ts"], T0);
    w.rig.clock.advance_secs(2 * 3_600 - 600);
    let s = w.rig.status().await;
    assert_eq!(s["heartbeat_age_secs"], 7_200);
    assert_eq!(s["degraded"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_any_heartbeat_the_age_is_null_and_degraded_comes_two_hours_after_start() {
    let w = world().await;
    let s = w.rig.status().await;
    assert!(s["heartbeat_age_secs"].is_null());
    assert_eq!(s["degraded"], false);
    w.rig.clock.advance_secs(2 * 3_600);
    assert_eq!(w.rig.status().await["degraded"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dead_man_switch_alerts_when_a_watcher_is_silent_for_30_minutes() {
    let w = world().await;
    let deadman = w.deadman();
    w.heartbeat_at(T0).await;
    w.watcher.poll().await;
    deadman.tick().await;
    // The watcher keeps polling: nothing goes out.
    for _ in 0..4 {
        w.rig.clock.advance_secs(600);
        w.heartbeat_at(w.rig.clock.now_secs()).await;
        w.watcher.poll().await;
        deadman.tick().await;
    }
    assert!(w.rig.sink.sent().is_empty());
    // The watcher stops.
    w.rig.clock.advance_secs(29 * 60);
    w.heartbeat_at(w.rig.clock.now_secs()).await;
    deadman.tick().await;
    assert!(w.rig.sink.sent().is_empty(), "29 minutes is not 30");
    w.rig.clock.advance_secs(60);
    deadman.tick().await;
    assert_eq!(w.alarm_keys(), vec!["watcher_silent:laptop"]);
    // It polls again: the alarm ends and a new silence is sent at once.
    w.watcher.poll().await;
    deadman.tick().await;
    w.rig.clock.advance_secs(30 * 60);
    deadman.tick().await;
    assert_eq!(w.rig.sink.sent().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dead_man_switch_warns_at_four_hours_without_a_heartbeat_and_when_status_is_down() {
    let w = world().await;
    let deadman = w.deadman();
    w.heartbeat_at(T0).await;
    w.watcher.poll().await;
    deadman.tick().await;
    w.rig.clock.advance_secs(4 * 3_600 - 60);
    w.watcher.poll().await;
    deadman.tick().await;
    assert!(w.rig.sink.sent().is_empty());
    w.rig.clock.advance_secs(60);
    w.watcher.poll().await;
    deadman.tick().await;
    assert_eq!(w.alarm_keys(), vec!["heartbeat_silent"]);

    // A status that does not answer is silence too.
    let down = Deadman::new(
        "http://127.0.0.1:1/v1/status",
        w.rig.clock.clone(),
        w.alarms.clone(),
    );
    down.tick().await;
    w.rig.clock.advance_secs(30 * 60);
    down.tick().await;
    assert!(w.alarm_keys().contains(&"operator_unreachable".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_apart_by_more_than_ten_blocks_or_in_leaves_at_one_block_alarm_on_the_second_poll(
) {
    let w = world().await;
    // Drift of a few blocks, with leaves that differ because the blocks differ: normal.
    w.rig.vault.edit(|v| (v.height, v.leaf_count) = (1_000, 3));
    w.second.edit(|v| (v.height, v.leaf_count) = (1_004, 4));
    for _ in 0..3 {
        w.watcher.poll().await;
    }
    assert!(w.rig.sink.sent().is_empty());
    // Eleven blocks apart: one poll is not enough.
    w.second.edit(|v| (v.height, v.leaf_count) = (1_011, 3));
    w.watcher.poll().await;
    assert!(w.rig.sink.sent().is_empty());
    w.watcher.poll().await;
    assert_eq!(w.alarm_keys(), vec!["providers_disagree"]);
    // Back in step: the next disagreement is sent at once.
    w.second.edit(|v| (v.height, v.leaf_count) = (1_001, 3));
    w.watcher.poll().await;
    // Same block, different leaves.
    w.second.edit(|v| (v.height, v.leaf_count) = (1_000, 5));
    w.watcher.poll().await;
    assert_eq!(w.rig.sink.sent().len(), 1);
    w.watcher.poll().await;
    assert_eq!(w.rig.sink.sent().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn three_new_exits_in_an_hour_raise_an_alarm_two_or_a_slow_trickle_do_not() {
    let w = world().await;
    let show = |ids: &[u32]| {
        let exits: Vec<_> = ids.iter().map(|i| exit(*i, u64::from(*i))).collect();
        let records: Vec<_> = ids.iter().map(|i| record(*i)).collect();
        w.both(move |v| {
            v.exits = exits.clone();
            v.records = records.clone();
        });
    };
    show(&[0]);
    w.watcher.poll().await;
    w.rig.clock.advance_secs(1_800);
    show(&[0, 1]);
    w.watcher.poll().await;
    w.watcher.poll().await;
    assert!(
        w.rig.sink.sent().is_empty(),
        "the same exit is counted once"
    );
    // The first one is 61 minutes old by now, so only two are in the window.
    w.rig.clock.advance_secs(1_860);
    show(&[0, 1, 2]);
    w.watcher.poll().await;
    assert!(w.rig.sink.sent().is_empty());
    w.rig.clock.advance_secs(60);
    show(&[0, 1, 2, 3]);
    w.watcher.poll().await;
    assert_eq!(w.alarm_keys(), vec!["exit_wave"]);
}

/// Three exits that were pending before a restart are not a wave: the database knows them, so a
/// new process does not count them as new.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_does_not_read_pending_exits_as_a_wave() {
    let w = world().await;
    let ids = [0u32, 1, 2];
    let exits: Vec<_> = ids.iter().map(|i| exit(*i, u64::from(*i))).collect();
    let records: Vec<_> = ids.iter().map(|i| record(*i)).collect();
    w.both(move |v| {
        v.exits = exits.clone();
        v.records = records.clone();
    });
    // Imported by an earlier process.
    for i in ids {
        let n = nullifier_hex(u64::from(i));
        w.rig
            .op
            .store()
            .run(move |c| store::record_exit_seen(c, &n))
            .await
            .unwrap();
    }
    // A fresh watcher over the same database, as after a restart.
    let providers: Vec<Arc<dyn VaultView>> = vec![w.rig.vault.clone(), w.second.clone()];
    let fresh = Watcher::new(
        providers,
        Arc::new(FakeCalls::default()),
        w.rig.op.store().clone(),
        w.rig.clock.clone(),
        w.alarms.clone(),
        WatcherConfig::default(),
    )
    .unwrap();
    fresh.poll().await;
    assert!(w.rig.sink.sent().is_empty(), "{:?}", w.alarm_keys());
}

struct Opener(Arc<FakeVault>);

#[async_trait]
impl OldStateExit for Opener {
    async fn open(&self) -> Result<u32, String> {
        self.0.edit(|v| {
            v.exits = vec![exit(0, 1)];
            v.records = vec![record(0)];
        });
        Ok(0)
    }
}

struct NoOpener;

#[async_trait]
impl OldStateExit for NoOpener {
    async fn open(&self) -> Result<u32, String> {
        Err("no test note".into())
    }
}

fn synthetic(w: &World, opener: Arc<dyn OldStateExit>) -> Synthetic {
    Synthetic::new(
        opener,
        w.rig.vault.clone(),
        w.rig.clock.clone(),
        w.alarms.clone(),
    )
    .with_timing(600, Duration::from_millis(1))
}

/// An opener that opens nothing (the note stays `Active`) must not pass the daily test: a
/// status other than `PendingExit` proves an answer only after the exit was seen open.
struct DoesNothing;

#[async_trait]
impl OldStateExit for DoesNothing {
    async fn open(&self) -> Result<u32, String> {
        Ok(0)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daily_test_does_not_pass_when_no_exit_was_ever_open() {
    let w = world().await;
    w.rig.vault.edit(|v| {
        v.records = vec![NoteRecord {
            status: "Active".into(),
            ..record(0)
        }];
    });
    let clock = w.rig.clock.clone();
    let ticker = tokio::spawn(async move {
        for _ in 0..20 {
            clock.advance_secs(60);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    assert!(!synthetic(&w, Arc::new(DoesNothing)).run_once().await);
    ticker.await.unwrap();
    assert_eq!(w.alarm_keys(), vec!["synthetic_failed"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daily_test_passes_when_the_exit_is_answered_and_alarms_when_it_is_not() {
    let w = world().await;
    // The watcher's answer: the note leaves its exit.
    let vault = w.rig.vault.clone();
    let answer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        vault.edit(|v| v.records[0].status = "Active".into());
    });
    assert!(
        synthetic(&w, Arc::new(Opener(w.rig.vault.clone())))
            .run_once()
            .await
    );
    answer.await.unwrap();
    assert!(w.rig.sink.sent().is_empty());

    // Nobody answers: the clock runs out.
    w.rig
        .vault
        .edit(|v| v.records[0].status = "PendingExit".into());
    let clock = w.rig.clock.clone();
    let ticker = tokio::spawn(async move {
        for _ in 0..20 {
            clock.advance_secs(60);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    assert!(
        !synthetic(&w, Arc::new(Opener(w.rig.vault.clone())))
            .run_once()
            .await
    );
    ticker.await.unwrap();
    assert_eq!(w.alarm_keys(), vec!["synthetic_failed"]);

    assert!(!synthetic(&w, Arc::new(NoOpener)).run_once().await);
    assert!(w
        .alarm_keys()
        .contains(&"synthetic_open_failed".to_string()));
}
