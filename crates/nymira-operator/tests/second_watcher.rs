//! The second machine's watcher answers exits from what was replicated, with the first watcher
//! never having run. Its store is the replica's own: it holds nothing else.

mod common;

use std::sync::Arc;

use common::*;
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::{ExitEntry, FakeVault, NoteRecord, VaultView};
use nymira_operator::replica::Replica;
use nymira_operator::replicator::Replicator;
use nymira_operator::replog::{Log, ReplicaNode};
use nymira_operator::store::Store;
use nymira_operator::watcher::{FakeCalls, Key, Watcher, WatcherConfig, GAS_DEFEAT, GAS_RAISE};
use serde_json::Value;

const DEADLINE_NS: u128 = (T0 as u128 + 7 * 86_400) * 1_000_000_000;

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

struct World {
    rig: Rig,
    replica_store: Store,
    vaults: Vec<Arc<FakeVault>>,
    calls: Arc<FakeCalls>,
    watcher: Watcher,
}

async fn world() -> World {
    let replica_store = Store::open_in_memory().unwrap();
    let node = Arc::new(ReplicaNode::new(
        Log::new(replica_store.clone()),
        ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
    ));
    let pk = node.public_key();
    let link = node.clone();
    let clock_secs = T0;
    let rig = Rig::new(Setup {
        replica: Some(Box::new(move |store| {
            Arc::new(Replicator::new(
                Log::new(store),
                link,
                pk,
                Arc::new(nymira_operator::clock::ManualClock::new(clock_secs * 1000)),
            )) as Arc<dyn Replica>
        })),
        ..Setup::default()
    })
    .await;
    let vaults: Vec<Arc<FakeVault>> = (0..3)
        .map(|_| Arc::new(FakeVault::new(vault_config(), &root_hex())))
        .collect();
    let calls = Arc::new(FakeCalls::default());
    let providers: Vec<Arc<dyn VaultView>> = vaults.iter().map(|v| v.clone() as _).collect();
    // The watcher of the second machine: the replica's store, a clock, nothing of the laptop's.
    let watcher = Watcher::new(
        providers,
        calls.clone(),
        replica_store.clone(),
        rig.clock.clone(),
        Arc::new(Alarms::new(vec![rig.sink.clone()], rig.clock.clone())),
        WatcherConfig {
            quorum: 2,
            ..WatcherConfig::default()
        },
    )
    .unwrap();
    World {
        rig,
        replica_store,
        vaults,
        calls,
        watcher,
    }
}

impl World {
    fn pending_exit(&self, e: ExitEntry) {
        for v in &self.vaults {
            let e = e.clone();
            v.edit(move |v| {
                v.exits = vec![e.clone()];
                v.records = vec![NoteRecord {
                    note_id: e.note_id,
                    deposit_amount: 2_000_000,
                    expiry: EXPIRY,
                    status: "PendingExit".into(),
                    leaf_retired: false,
                }];
                v.leaf_count = 1;
            });
        }
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
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_first_watcher_stopped_the_second_defeats_a_stale_exit_from_replicated_data() {
    let w = world().await;
    let lease = w.open_lease(2, false).await;
    // The laptop's own store is not consulted: the proof the watcher answers with comes from
    // the replica's copy.
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!((seen[0].key, seen[0].method), (Key::Answer, "defeat_exit"));
    assert_eq!(seen[0].gas, GAS_DEFEAT);
    let id = lease.clone();
    let json = w
        .replica_store
        .run(move |c| nymira_operator::store::request_json(c, &id))
        .await
        .unwrap()
        .unwrap();
    let request: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(seen[0].args["proof"], request["proof"]["proof"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_the_laptop_replicated_lets_the_second_watcher_raise_the_concession() {
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
    assert_eq!(seen[0].args["amount"], "100000");
    assert_eq!(seen[0].gas, GAS_RAISE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_whose_expire_was_refused_still_gets_its_concession_raised() {
    let w = world().await;
    let (s, v, _) = w.rig.lease(&w.rig.fresh(1)).await;
    assert_eq!(s, 201, "{v}");
    let (id, token) = (
        v["lease_id"].as_str().unwrap().to_string(),
        v["lease_token"].as_str().unwrap().to_string(),
    );
    w.rig.op.mark_delivered(&id).await.unwrap();
    // The client gives up the lease; the operator refuses, since the key was used.
    let response = w
        .rig
        .http
        .post(format!("{}/v2/openrouter/leases/{id}/expire", w.rig.url))
        .header("x-nymira-lease-token", &token)
        .send()
        .await
        .unwrap();
    let (status, body) = (
        response.status().as_u16(),
        response.json::<Value>().await.unwrap(),
    );
    json_error(status, &body, 409, "lease_used");
    // The old state is filed as an exit: the proof kept by the refusal is what answers it.
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    let seen = w.calls.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].key, seen[0].method),
        (Key::Answer, "raise_concession")
    );
    assert_eq!(seen[0].args["amount"], "100000");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reservation_that_was_rolled_back_leaves_no_proof_on_the_replica() {
    let w = world().await;
    w.rig
        .provider
        .set_mint_error(Some(nymira_operator::provider::ProviderError::Unavailable(
            "down".into(),
        )));
    let (s, v, _) = w.rig.lease(&w.rig.fresh(2)).await;
    json_error(s, &v, 502, "provider_error");
    w.pending_exit(exit(0, 1, 2));
    w.watcher.poll().await;
    assert!(w.calls.seen().is_empty(), "nothing to answer with");
}
