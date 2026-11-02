//! What the laptop keeps after a lease ends, the retention job, the reconcile rate limit and the
//! alarm on a sick replica.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use nymira_operator::replica::{Replica, ReplicaEntry, ReplicaError};
use nymira_operator::replog::RETENTION_SECS;
use serde_json::json;

const MODEL: &str = "z-ai/glm-5.3-flash";

async fn settled_lease(rig: &Rig) -> String {
    let (s, lease, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201, "{lease}");
    let (id, token) = (
        lease["lease_id"].as_str().unwrap().to_string(),
        lease["lease_token"].as_str().unwrap().to_string(),
    );
    let chat = json!({"model": MODEL, "messages": [{"role": "user", "content": "ab".repeat(80)}],
        "max_tokens": 10, "stream": false});
    let r = rig
        .http
        .post(format!("{}/v1/chat/completions", rig.url))
        .header("x-signing-algo", "ed25519")
        .header("x-client-pub-key", "11".repeat(32))
        .header("x-model-pub-key", "22".repeat(32))
        .header("x-encryption-version", "2")
        .header("x-nymira-lease-id", &id)
        .header("x-nymira-lease-token", &token)
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    for _ in 0..4 {
        rig.clock.advance_secs(700);
        rig.op.tick().await;
    }
    id
}

type Row = (
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    i64,
    i64,
    String,
);

async fn row(rig: &Rig, id: &str) -> Row {
    let id = id.to_string();
    rig.op
        .store()
        .run(move |c| {
            c.query_row(
                "SELECT key_id, last_activity, revoked_at, usage_json, settled_at, issued_at, status
                 FROM leases WHERE lease_id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn settlement_deletes_the_key_id_and_the_exact_timers() {
    let rig = Rig::new(Setup::default()).await;
    let id = settled_lease(&rig).await;
    let (key_id, activity, revoked, usage, settled_at, issued_at, status) = row(&rig, &id).await;
    assert_eq!(status, "settled");
    assert!(key_id.is_none() && activity.is_none() && revoked.is_none() && usage.is_some());
    assert_eq!(settled_at % 3600, 0);
    assert_eq!(issued_at % 3600, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn old_request_proofs_and_signed_states_are_purged_on_a_schedule() {
    let rig = Rig::new(Setup::default()).await;
    let id = settled_lease(&rig).await;
    let count = |rig: &Rig, sql: &'static str| {
        let store = rig.op.store().clone();
        async move {
            store
                .run(move |c| c.query_row(sql, [], |r| r.get::<_, i64>(0)))
                .await
                .unwrap()
        }
    };
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM signed_states").await, 1);
    assert_eq!(
        count(
            &rig,
            "SELECT COUNT(*) FROM leases WHERE request_json != '{}'"
        )
        .await,
        1
    );
    rig.clock.advance_secs(RETENTION_SECS + 7_200);
    rig.op.tick().await;
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM signed_states").await, 0);
    assert_eq!(
        count(
            &rig,
            "SELECT COUNT(*) FROM leases WHERE request_json != '{}'"
        )
        .await,
        0
    );
    // The row itself and its charge stay: the daily budget and the evidence still read them.
    assert_eq!(row(&rig, &id).await.6, "settled");
}

#[tokio::test(flavor = "multi_thread")]
async fn reconcile_is_limited_across_all_callers() {
    let rig = Rig::new(Setup::default()).await;
    for n in 0..30 {
        let (s, _) = rig.post_json("/v2/reconcile", b"{}".to_vec()).await;
        assert_eq!(s, 400, "call {n}");
    }
    let (s, v) = rig.post_json("/v2/reconcile", b"{}".to_vec()).await;
    json_error(s, &v, 429, "operator_capacity");
    rig.clock.advance_secs(61);
    let (s, _) = rig.post_json("/v2/reconcile", b"{}".to_vec()).await;
    assert_eq!(s, 400);
}

struct Sick(&'static str);

#[async_trait]
impl Replica for Sick {
    async fn append(&self, _: &ReplicaEntry) -> Result<(), ReplicaError> {
        Ok(())
    }
    fn health(&self) -> &'static str {
        self.0
    }
    fn is_real(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diverged_or_unreachable_replica_raises_an_alarm() {
    for health in ["diverged", "unreachable"] {
        let rig = Rig::new(Setup {
            replica: Some(Box::new(move |_| Arc::new(Sick(health)))),
            ..Setup::default()
        })
        .await;
        rig.clock.advance_secs(11);
        rig.op.tick().await;
        let alarms = rig.sink.sent();
        assert!(
            alarms.iter().any(|a| a.key == format!("replica_{health}")),
            "{health}: {alarms:?}"
        );
        assert_eq!(rig.status().await["replica"], health);
    }
}
