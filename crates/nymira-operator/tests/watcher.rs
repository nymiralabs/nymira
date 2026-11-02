//! The watcher against scripted providers and the operator's own store.

mod common;

use std::sync::Arc;

use common::*;
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::{ExitEntry, FakeVault, NoteRecord, VaultView};
use nymira_operator::store;
use nymira_operator::watcher::{
    FakeCalls, Key, Watcher, WatcherConfig, GAS_CLAIM_BATCH, GAS_DEFEAT, GAS_RAISE, GAS_RETIRE,
    GAS_SYNC,
};
use serde_json::Value;
use zkapi_core::MerkleTree;
use zkapi_types::Felt252;

const DEADLINE_NS: u128 = (T0 as u128 + 7 * 86_400) * 1_000_000_000;

struct World {
    rig: Rig,
    second: Arc<FakeVault>,
    third: Arc<FakeVault>,
    calls: Arc<FakeCalls>,
    watcher: Watcher,
}

fn record(id: u32, status: &str, expiry: u64, retired: bool) -> NoteRecord {
    NoteRecord {
        note_id: id,
        deposit_amount: 2_000_000,
        expiry,
        status: status.into(),
        leaf_retired: retired,
    }
}

fn exit(note_id: u32, a: u64, b: u64) -> ExitEntry {
    ExitEntry {
        note_id,
        nullifier: nullifier_hex(a),
        next_nullifier: nullifier_hex(b),
        final_balance: 1_990_000,
        conceded: 0,
        deadline: DEADLINE_NS,
    }
}

async fn world_with(quorum: usize) -> World {
    let rig = Rig::new(Setup::default()).await;
    let second = Arc::new(FakeVault::new(vault_config(), &root_hex()));
    let third = Arc::new(FakeVault::new(vault_config(), &root_hex()));
    let calls = Arc::new(FakeCalls::default());
    let providers: Vec<Arc<dyn VaultView>> = vec![rig.vault.clone(), second.clone(), third.clone()];
    let watcher = Watcher::new(
        providers,
        calls.clone(),
        rig.op.store().clone(),
        rig.clock.clone(),
        Arc::new(Alarms::new(vec![rig.sink.clone()], rig.clock.clone())),
        WatcherConfig {
            quorum,
            ..WatcherConfig::default()
        },
    )
    .unwrap();
    World {
        rig,
        second,
        third,
        calls,
        watcher,
    }
}

async fn world() -> World {
    world_with(2).await
}

impl World {
    /// Every provider shows the same chain.
    fn all(&self, f: impl Fn(&mut nymira_operator::chain::FakeVaultState)) {
        for v in [&self.rig.vault, &self.second, &self.third] {
            v.edit(&f);
        }
    }

    fn pending_exit(&self, e: ExitEntry) {
        self.all(|v| {
            v.exits = vec![e.clone()];
            v.records = vec![record(e.note_id, "PendingExit", EXPIRY, false)];
            v.leaf_count = 1;
        });
    }

    async fn open_lease(&self, n: u64, delivered: bool) -> String {
        let (s, v, _) = self.rig.lease(&self.rig.fresh(n)).await;
        assert_eq!(s, 201, "{v}");
        let id = v["lease_id"].as_str().unwrap().to_string();
        if delivered {
            self.rig.op.mark_delivered(&id).await.unwrap();
        }
        id
    }

    fn answers(&self) -> usize {
        self.calls
            .seen()
            .iter()
            .filter(|c| c.key == Key::Answer)
            .count()
    }

    fn methods(&self) -> Vec<&'static str> {
        self.calls.seen().iter().map(|c| c.method).collect()
    }
}

fn no_close(w: &World) {
    assert!(
        !w.methods().contains(&"mutual_close"),
        "the watcher never sends a close"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_proof_at_the_successor_defeats_the_exit() {
    let w = world().await;
    let lease = w.open_lease(2, false).await;
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!((seen[0].key, seen[0].method), (Key::Answer, "defeat_exit"));
    assert_eq!(seen[0].args["note_id"], 0);
    assert_eq!(seen[0].args["inputs"].as_array().unwrap().len(), 12);
    let id = lease.clone();
    let stored: nymira_operator::replica::ReplicaEntry = {
        let json = w
            .rig
            .op
            .store()
            .run(move |c| store::request_json(c, &id))
            .await
            .unwrap()
            .unwrap();
        nymira_operator::replica::ReplicaEntry {
            nullifier: String::new(),
            lease_id: lease,
            request_json: json,
        }
    };
    let request: Value = serde_json::from_str(&stored.request_json).unwrap();
    assert_eq!(seen[0].args["proof"], request["proof"]["proof"]);
    assert!(seen[0].gas <= 300_000_000_000_000 && seen[0].gas == GAS_DEFEAT);
    no_close(&w);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivered_lease_at_the_escaped_nullifier_raises_the_concession_by_its_bound() {
    let w = world().await;
    w.open_lease(1, true).await;
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].key, seen[0].method),
        (Key::Answer, "raise_concession")
    );
    assert_eq!(seen[0].args["amount"], "100000", "the lease's bound");
    assert_eq!(seen[0].gas, GAS_RAISE);
    no_close(&w);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_that_would_not_raise_the_concession_is_skipped() {
    let w = world().await;
    w.open_lease(1, true).await;
    let mut e = exit(0, 1, 2);
    e.conceded = 100_000;
    e.final_balance = 1_890_000;
    w.pending_exit(e.clone());
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "already conceded the bound");
    // One provider that is behind does not hide the concession from the others.
    e.conceded = 50_000;
    e.final_balance = 1_940_000;
    w.pending_exit(e.clone());
    w.rig.vault.edit(|v| v.exits[0].conceded = 100_000);
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "the highest reading wins");
    w.rig.vault.edit(|v| v.exits[0].conceded = 50_000);
    w.rig.clock.advance_secs(61);
    w.watcher.poll().await;
    assert_eq!(w.methods(), vec!["raise_concession"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_with_no_proof_to_answer_with_is_left_alone() {
    // No lease at all; a lease nothing was proxied under; a lease closed by expire.
    let w = world().await;
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "a legitimate exit");

    let w = world().await;
    w.open_lease(1, false).await;
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "no request was proxied under it");

    let w = world().await;
    let (s, v, _) = w.rig.lease(&w.rig.fresh(1)).await;
    assert_eq!(s, 201);
    let expired = w
        .rig
        .http
        .post(format!(
            "{}/v2/openrouter/leases/{}/expire",
            w.rig.url,
            v["lease_id"].as_str().unwrap()
        ))
        .header("x-nymira-lease-token", v["lease_token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 200);
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    assert!(
        w.calls.seen().is_empty(),
        "an expired lease leaves no proof"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_that_two_providers_do_not_both_report_is_not_answered() {
    let w = world().await;
    w.open_lease(2, false).await;
    w.rig.vault.edit(|v| {
        v.exits = vec![exit(0, 1, 2)];
        v.records = vec![record(0, "PendingExit", EXPIRY, false)];
        v.leaf_count = 1;
    });
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "one provider alone");
    let n = nullifier_hex(1);
    let row = w
        .rig
        .op
        .store()
        .run(move |c| store::nullifier_row(c, &n))
        .await
        .unwrap();
    assert!(row.is_none(), "and nothing joins the blocked set");

    w.second.edit(|v| {
        v.exits = vec![exit(0, 1, 2)];
        v.records = vec![record(0, "PendingExit", EXPIRY, false)];
        v.leaf_count = 1;
    });
    w.watcher.poll().await;
    assert_eq!(w.methods(), vec!["defeat_exit"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_call_goes_against_a_closed_or_swept_note() {
    let w = world().await;
    w.open_lease(2, false).await;
    w.all(|v| {
        v.exits = vec![exit(0, 1, 2)];
        v.records = vec![record(0, "Closed", EXPIRY, true)];
        v.leaf_count = 1;
    });
    w.watcher.poll().await;
    assert!(
        !w.methods().contains(&"defeat_exit"),
        "{:?}",
        w.calls.seen()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confirmed_exit_blocks_its_nullifiers_for_leases() {
    let w = world().await;
    w.pending_exit(exit(0, 20, 21));
    w.watcher.poll().await;
    for n in [20, 21] {
        let (s, v, _) = w.rig.lease(&w.rig.fresh(n)).await;
        json_error(s, &v, 409, "nullifier_in_exit");
    }
    let (s, v) = w.rig.clearance(&w.rig.ownership(20)).await;
    json_error(s, &v, 409, "nullifier_in_exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_answers_are_counted_alarm_at_three_and_need_every_provider_at_ten() {
    let w = world().await;
    w.open_lease(2, false).await;
    w.pending_exit(exit(0, 1, 2));
    w.calls.fail(&["defeat_exit"]);
    for _ in 0..3 {
        w.watcher.poll().await;
        w.rig.clock.advance_secs(61);
    }
    assert_eq!(w.answers(), 3);
    assert_eq!(
        w.rig.sink.sent().len(),
        1,
        "the third failure raises one alarm"
    );
    for _ in 0..7 {
        w.watcher.poll().await;
        w.rig.clock.advance_secs(61);
    }
    assert_eq!(w.answers(), 10);
    // Ten failures in the hour: two providers are no longer enough.
    w.third.edit(|v| v.exits.clear());
    w.watcher.poll().await;
    assert_eq!(w.answers(), 10, "the third provider does not show it");
    w.third.edit(|v| v.exits = vec![exit(0, 1, 2)]);
    w.watcher.poll().await;
    assert_eq!(w.answers(), 11, "all three agree");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_low_exit_answer_balance_raises_an_alarm() {
    let w = world().await;
    w.watcher.poll().await;
    assert!(w.rig.sink.sent().is_empty());
    w.calls.set_balance(1_900_000_000_000_000_000_000_000);
    w.watcher.poll().await;
    let sent = w.rig.sink.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].body.contains("1.900 NEAR"), "{}", sent[0].body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_low_vault_liquid_balance_raises_an_alarm_and_a_top_up_clears_it() {
    let w = world().await;
    w.watcher.poll().await;
    assert!(w.rig.sink.sent().is_empty());
    w.rig
        .vault
        .edit(|v| v.liquid_near = 2_500_000_000_000_000_000_000_000);
    w.watcher.poll().await;
    let sent = w.rig.sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].key, "vault_liquid_low");
    assert!(sent[0].body.contains("2.500 NEAR"), "{}", sent[0].body);
    // Still low: inside the repeat window nothing more goes out.
    w.watcher.poll().await;
    assert_eq!(w.rig.sink.sent().len(), 1);
    w.rig
        .vault
        .edit(|v| v.liquid_near = 5_000_000_000_000_000_000_000_000);
    w.watcher.poll().await;
    w.rig
        .vault
        .edit(|v| v.liquid_near = 2_000_000_000_000_000_000_000_000);
    w.watcher.poll().await;
    assert_eq!(w.rig.sink.sent().len(), 2, "resolved, then raised again");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_undefeated_exit_is_finalized_after_its_deadline_and_not_answered() {
    let w = world().await;
    w.open_lease(2, false).await;
    w.pending_exit(exit(0, 1, 2));
    w.rig.clock.advance_secs(7 * 86_400 - 1);
    w.watcher.poll().await;
    assert!(
        !w.methods().contains(&"finalize_exit"),
        "{:?}",
        w.calls.seen()
    );
    // The defeat sent a moment ago pauses the note for 30 seconds.
    w.rig.clock.advance_secs(40);
    w.watcher.poll().await;
    let seen = w.calls.seen();
    let last = seen.last().unwrap();
    assert_eq!((last.key, last.method), (Key::Upkeep, "finalize_exit"));
    assert_eq!(last.args["note_id"], 0);
    assert!(
        !w.methods()[1..].contains(&"defeat_exit"),
        "too late to defeat"
    );
}

fn leaf(n: u64) -> Felt252 {
    Felt252::from_u64(n + 77)
}

fn hex_of(f: &Felt252) -> String {
    format!("0x{}", hex::encode(f.as_bytes()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_leaf_is_retired_with_the_siblings_a_client_rebuilds() {
    let w = world().await;
    let leaves: Vec<Felt252> = (0..3).map(leaf).collect();
    w.all(|v| {
        v.records = vec![
            record(0, "Closed", EXPIRY, false),
            record(1, "Active", EXPIRY, false),
            record(2, "Closed", EXPIRY, true),
        ];
        v.leaf_count = 3;
        v.leaves = leaves.iter().map(hex_of).collect();
    });
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].key, seen[0].method),
        (Key::Upkeep, "retire_closed_leaf")
    );
    assert_eq!(seen[0].args["note_id"], 0);
    let level = (31 - 3u32.leading_zeros()) as usize;
    let expected: Vec<String> = MerkleTree::from_leaves(&leaves).get_siblings(0)[..level]
        .iter()
        .map(hex_of)
        .collect();
    assert_eq!(seen[0].args["siblings"], serde_json::json!(expected));
    assert_eq!(seen[0].gas, GAS_RETIRE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_note_not_in_the_tree_yet_is_not_retired() {
    let w = world().await;
    w.all(|v| {
        v.records = vec![record(0, "Closed", EXPIRY, false)];
        v.leaf_count = 0;
    });
    w.watcher.poll().await;
    assert!(!w.methods().contains(&"retire_closed_leaf"));
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_notes_are_claimed_in_a_batch_and_one_by_one_when_the_batch_fails() {
    let w = world().await;
    let old = T0 - 8 * 86_400;
    w.all(|v| {
        v.records = vec![
            record(0, "Active", old, false),
            record(1, "Active", EXPIRY, false),
            record(2, "Active", old, false),
            record(3, "Closed", old, true),
        ];
        v.leaf_count = 4;
    });
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].method, "claim_expired_batch");
    assert_eq!(seen[0].args, serde_json::json!({"from": 0, "limit": 50}));
    assert!(seen[0].gas == GAS_CLAIM_BATCH && seen[0].gas <= 300_000_000_000_000);
    w.watcher.poll().await;
    assert_eq!(w.calls.seen().len(), 1, "not asked again within the hour");

    let w = world().await;
    w.all(|v| {
        v.records = vec![
            record(0, "Active", old, false),
            record(1, "Active", EXPIRY, false),
            record(2, "Active", old, false),
        ];
        v.leaf_count = 3;
    });
    w.calls.fail(&["claim_expired_batch"]);
    w.watcher.poll().await;
    assert_eq!(
        w.methods(),
        vec!["claim_expired_batch", "claim_expired", "claim_expired"]
    );
    let ids: Vec<Value> = w.calls.seen()[1..]
        .iter()
        .map(|c| c.args["note_id"].clone())
        .collect();
    assert_eq!(ids, vec![serde_json::json!(0), serde_json::json!(2)]);
    w.rig.clock.advance_secs(3_601);
    w.calls.fail(&[]);
    w.watcher.poll().await;
    assert_eq!(
        w.methods().last(),
        Some(&"claim_expired_batch"),
        "asked again after the hour"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_leaves_are_synced_at_most_every_thirty_seconds() {
    let w = world().await;
    w.all(|v| {
        v.records = vec![
            record(0, "Active", EXPIRY, false),
            record(1, "Active", EXPIRY, false),
            record(2, "Active", EXPIRY, false),
        ];
        v.leaf_count = 2;
        v.leaves = vec![hex_of(&leaf(0)), hex_of(&leaf(1))];
    });
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].key, seen[0].method), (Key::Upkeep, "sync_root"));
    assert_eq!(seen[0].args["max"], 80);
    assert_eq!(seen[0].gas, GAS_SYNC);
    w.watcher.poll().await;
    assert_eq!(w.calls.seen().len(), 1);
    w.rig.clock.advance_secs(30);
    w.watcher.poll().await;
    assert_eq!(w.calls.seen().len(), 2);
    w.all(|v| v.leaf_count = 3);
    w.rig.clock.advance_secs(60);
    w.watcher.poll().await;
    assert_eq!(w.calls.seen().len(), 2, "nothing waits");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watcher_needs_as_many_providers_as_its_quorum() {
    let w = world().await;
    let one: Vec<Arc<dyn VaultView>> = vec![w.rig.vault.clone()];
    let refused = Watcher::new(
        one,
        w.calls.clone(),
        w.rig.op.store().clone(),
        w.rig.clock.clone(),
        Arc::new(Alarms::new(vec![], w.rig.clock.clone())),
        WatcherConfig::default(),
    );
    assert!(refused.is_err());
}
