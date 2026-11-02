//! A lost disk or an old backup: the operator comes back from the replica, signs nothing until
//! it has, and an honest client settles its in-flight lease through `POST /v2/reconcile`.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use ed25519_dalek::SigningKey;
use nymira_operator::lifecycle::UNREAD_SETTLES_AFTER_SECS;
use nymira_operator::replica::Replica;
use nymira_operator::replicator::Replicator;
use nymira_operator::replog::{Entry, Link, LinkError, Log, ReplicaNode, SignedHead};
use nymira_operator::store::{self, Store};
use serde_json::{json, Value};

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

struct Wire {
    node: Arc<ReplicaNode>,
    down: Mutex<bool>,
    /// Alters the payload of the first entry a fetch returns.
    tamper: Mutex<bool>,
}

impl Wire {
    fn node(&self) -> Result<&ReplicaNode, LinkError> {
        if *self.down.lock().unwrap() {
            return Err(LinkError::Down("cable out".into()));
        }
        Ok(&self.node)
    }
}

#[async_trait]
impl Link for Wire {
    async fn append(&self, e: &Entry) -> Result<SignedHead, LinkError> {
        Link::append(self.node()?, e).await
    }
    async fn head(&self) -> Result<SignedHead, LinkError> {
        Link::head(self.node()?).await
    }
    async fn hashes(&self, s: &[u64]) -> Result<Vec<Option<String>>, LinkError> {
        Link::hashes(self.node()?, s).await
    }
    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError> {
        let mut entries = Link::fetch(self.node()?, from, limit).await?;
        if *self.tamper.lock().unwrap() {
            if let Some(first) = entries.first_mut() {
                first.payload = Some("{\"lease_id\":\"forged\"}".into());
            }
        }
        Ok(entries)
    }
}

fn wire() -> Arc<Wire> {
    Arc::new(Wire {
        node: Arc::new(ReplicaNode::new(
            Log::new(Store::open_in_memory().unwrap()),
            key(),
        )),
        down: Mutex::new(false),
        tamper: Mutex::new(false),
    })
}

/// A setup whose replica is the real replicator over `wire`, on the operator's own store.
fn real(wire: &Arc<Wire>, clock_secs: u64) -> Setup {
    let wire = wire.clone();
    Setup {
        replica: Some(Box::new(move |store| {
            Arc::new(Replicator::new(
                Log::new(store),
                wire,
                key().verifying_key().to_bytes(),
                Arc::new(nymira_operator::clock::ManualClock::new(clock_secs * 1000)),
            )) as Arc<dyn Replica>
        })),
        clock_secs,
        ..Setup::default()
    }
}

async fn open(rig: &Rig, n: u64) -> (String, String) {
    let (status, body, _) = rig.lease(&rig.fresh(n)).await;
    assert_eq!(status, 201, "{body}");
    (
        body["lease_id"].as_str().unwrap().into(),
        body["lease_token"].as_str().unwrap().into(),
    )
}

async fn nullifier(rig: &Rig, n: u64) -> Value {
    rig.get_json(&format!("/v2/nullifiers/{}", nullifier_hex(n)))
        .await
        .1
}

async fn lease(rig: &Rig, id: &str) -> store::LeaseRow {
    let id = id.to_string();
    rig.op
        .store()
        .run(move |c| store::lease(c, &id))
        .await
        .unwrap()
        .unwrap()
}

/// One lease settled with a signed state and one still open, on a replica-backed operator.
async fn two_leases(rig: &Rig) -> ((String, String), String) {
    let (done, token) = open(rig, 1).await;
    rig.provider.set_usage(&done, 40_000_000);
    let response = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{done}", rig.url))
        .header("x-nymira-lease-token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    for _ in 0..3 {
        rig.op.tick().await;
    }
    assert_eq!(nullifier(rig, 1).await["nullifier_status"], "finalized");
    let (open_id, _) = open(rig, 2).await;
    ((done, token), open_id)
}

fn wipe(rig: &Rig) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(rig.dir.path().join(format!("operator.db{suffix}")));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_disk_comes_back_from_the_replica_and_an_honest_client_reconciles() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let ((_done, _), open_id) = two_leases(&rig).await;
    let request = serde_json::from_slice::<Value>(&body(&rig.fresh(2))).unwrap();

    wipe(&rig);
    rig.restart(real(&wire, T0 + 100)).await;

    // The operator is level again and says so.
    assert_eq!(rig.status().await["recovery_mode"], false);
    // The settled lease keeps its signed state; the open one is blocked and waits.
    let v = nullifier(&rig, 1).await;
    assert_eq!(v["nullifier_status"], "finalized");
    assert!(v["request_response"].is_object());
    assert_eq!(nullifier(&rig, 2).await["nullifier_status"], "reserved");
    let restored = lease(&rig, &open_id).await;
    assert_eq!(restored.status, "restored");
    assert!(restored.token_hash.is_empty(), "no token comes back");
    // Its provider key is gone, since nobody holds the record of it.
    assert!(!rig.provider.is_live(&open_id));
    // Nothing new can be leased on that nullifier.
    let (s, v, _) = rig.lease(&rig.fresh(2)).await;
    assert_ne!(s, 201, "{v}");

    // A client that holds another request cannot reconcile it.
    let other = serde_json::from_slice::<Value>(&body(&rig.fresh(3))).unwrap();
    let (s, v) = rig
        .post_json(
            "/v2/reconcile",
            json!({ "lease_id": open_id, "request": other })
                .to_string()
                .into_bytes(),
        )
        .await;
    json_error(s, &v, 422, "proof_invalid");
    // The one who holds the request settles at the bound, and gets no next state.
    let (s, v) = rig
        .post_json(
            "/v2/reconcile",
            json!({ "lease_id": open_id, "request": request })
                .to_string()
                .into_bytes(),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["charge_applied"], "100000");
    assert!(v["next_state"].is_null());
    assert_eq!(nullifier(&rig, 2).await["nullifier_status"], "finalized");
    assert_eq!(
        lease(&rig, &open_id).await.charge_basis.as_deref(),
        Some("reconciled")
    );
    // Once is enough.
    let (s, _) = rig
        .post_json(
            "/v2/reconcile",
            json!({ "lease_id": open_id, "request": request })
                .to_string()
                .into_bytes(),
        )
        .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_backup_is_brought_level_with_what_the_replica_saw_after_it() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let (first, _) = open(&rig, 1).await;
    let backup = rig.dir.path().join("backup.db");
    let target = backup.to_string_lossy().to_string();
    rig.op
        .store()
        .run(move |c| c.execute("VACUUM INTO ?1", [target]).map(|_| ()))
        .await
        .unwrap();
    let (second, _) = open(&rig, 2).await;

    wipe(&rig);
    std::fs::copy(&backup, rig.dir.path().join("operator.db")).unwrap();
    rig.restart(real(&wire, T0 + 50)).await;

    assert_eq!(rig.status().await["recovery_mode"], false);
    // The lease the backup knew is as it was; the one it missed is blocked, not forgotten.
    assert_eq!(lease(&rig, &first).await.status, "active");
    assert_eq!(lease(&rig, &second).await.status, "restored");
    assert_eq!(nullifier(&rig, 2).await["nullifier_status"], "reserved");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_replica_out_of_reach_a_lost_disk_stays_in_recovery_and_signs_nothing() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let (open_id, _) = open(&rig, 1).await;
    wipe(&rig);
    *wire.down.lock().unwrap() = true;
    rig.restart(real(&wire, T0 + 10)).await;

    // Whether it is in recovery or merely cannot reach the replica, nothing is signed.
    let (s, v, _) = rig.lease(&rig.fresh(5)).await;
    assert!(s == 503, "{s} {v}");
    let (s, v) = rig.clearance(&rig.ownership(6)).await;
    assert!(s == 503, "{s} {v}");

    *wire.down.lock().unwrap() = false;
    rig.op.tick().await;
    assert_eq!(rig.status().await["recovery_mode"], false);
    assert_eq!(lease(&rig, &open_id).await.status, "restored");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_lease_nobody_reconciles_settles_at_its_bound_after_six_hours() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let (open_id, _) = open(&rig, 1).await;
    wipe(&rig);
    rig.restart(real(&wire, T0 + 100)).await;
    rig.clock.advance_secs(UNREAD_SETTLES_AFTER_SECS - 200);
    rig.op.tick().await;
    assert_eq!(lease(&rig, &open_id).await.status, "restored");
    rig.clock.advance_secs(300);
    rig.op.tick().await;
    let row = lease(&rig, &open_id).await;
    assert_eq!(
        (row.status.as_str(), row.charge, row.charge_basis.as_deref()),
        ("settled", Some(100_000), Some("recovered"))
    );
    assert_eq!(nullifier(&rig, 1).await["nullifier_status"], "finalized");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tampered_fetch_changes_nothing_in_the_database() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let (open_id, _) = open(&rig, 1).await;
    wipe(&rig);
    *wire.tamper.lock().unwrap() = true;
    rig.restart(real(&wire, T0 + 10)).await;
    // Still restoring: the operator refuses to sign and the database holds nothing of it.
    let (s, v, _) = rig.lease(&rig.fresh(5)).await;
    assert_eq!(s, 503, "{v}");
    let leases = rig.op.store().run(|c| store::all_leases(c)).await.unwrap();
    assert!(leases.is_empty(), "{leases:?}");
    *wire.tamper.lock().unwrap() = false;
    rig.op.tick().await;
    assert_eq!(lease(&rig, &open_id).await.status, "restored");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_that_stopped_before_stamping_the_lease_starts_its_clock_later() {
    let wire = wire();
    let mut rig = Rig::new(real(&wire, T0)).await;
    let (open_id, _) = open(&rig, 1).await;
    wipe(&rig);
    rig.restart(real(&wire, T0 + 10)).await;
    let id = open_id.clone();
    rig.op
        .store()
        .run(move |c| {
            c.execute(
                "UPDATE leases SET retiring_at = NULL WHERE lease_id = ?1",
                [id],
            )
            .map(|_| ())
        })
        .await
        .unwrap();
    rig.clock.advance_secs(UNREAD_SETTLES_AFTER_SECS + 5);
    rig.op.tick().await;
    assert_eq!(
        lease(&rig, &open_id).await.status,
        "restored",
        "the clock starts now"
    );
    rig.clock.advance_secs(UNREAD_SETTLES_AFTER_SECS + 5);
    rig.op.tick().await;
    assert_eq!(lease(&rig, &open_id).await.status, "settled");
}
