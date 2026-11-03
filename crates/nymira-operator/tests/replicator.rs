//! The laptop's end of replication against a real replica node: what is released only after the
//! replica holds it, what happens when the replica is away, lost its volume, or is ahead.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use nymira_operator::clock::ManualClock;
use nymira_operator::replica::{Replica, ReplicaEntry};
use nymira_operator::replicator::{Replicator, State};
use nymira_operator::replog::{kinds, Entry, Link, LinkError, Log, ReplicaNode, SignedHead};
use nymira_operator::store::Store;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn log() -> Log {
    Log::new(Store::open_in_memory().unwrap())
}

/// A link to a replica that can be switched off or pointed at another node.
struct Wire {
    node: Mutex<Arc<ReplicaNode>>,
    down: Mutex<bool>,
}

impl Wire {
    fn new(node: Arc<ReplicaNode>) -> Arc<Self> {
        Arc::new(Self {
            node: Mutex::new(node),
            down: Mutex::new(false),
        })
    }
    fn node(&self) -> Result<Arc<ReplicaNode>, LinkError> {
        if *self.down.lock().unwrap() {
            return Err(LinkError::Down("cable out".into()));
        }
        Ok(self.node.lock().unwrap().clone())
    }
    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
    fn swap(&self, node: Arc<ReplicaNode>) {
        *self.node.lock().unwrap() = node;
    }
}

#[async_trait]
impl Link for Wire {
    async fn append(&self, e: &Entry) -> Result<SignedHead, LinkError> {
        Link::append(&*self.node()?, e).await
    }
    async fn head(&self) -> Result<SignedHead, LinkError> {
        Link::head(&*self.node()?).await
    }
    async fn hashes(&self, s: &[u64]) -> Result<Vec<Option<String>>, LinkError> {
        Link::hashes(&*self.node()?, s).await
    }
    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError> {
        Link::fetch(&*self.node()?, from, limit).await
    }
}

struct Rig {
    laptop: Replicator,
    wire: Arc<Wire>,
    replica: Arc<ReplicaNode>,
}

fn rig() -> Rig {
    let replica = Arc::new(ReplicaNode::new(log(), key(1)));
    let wire = Wire::new(replica.clone());
    let laptop = Replicator::new(
        log(),
        wire.clone(),
        key(1).verifying_key().to_bytes(),
        Arc::new(ManualClock::new(7_200_000)),
    );
    Rig {
        laptop,
        wire,
        replica,
    }
}

fn reservation(n: u32) -> ReplicaEntry {
    ReplicaEntry {
        nullifier: format!("0x{n:064x}"),
        lease_id: format!("lease{n}"),
        request_json: "{\"proof\":1}".into(),
        bound: 100_000,
        client_request_id: format!("c{n}"),
        reserved_at: 7_200,
    }
}

#[tokio::test]
async fn a_reservation_is_on_the_replica_when_the_call_returns() {
    let r = rig();
    r.laptop.append(&reservation(1)).await.unwrap();
    let held = r.replica.from(1, 10).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].kind.as_str(), held[0].key.as_str()),
        ("reservation", "lease1")
    );
    assert!(held[0].payload.as_deref().unwrap().contains("proof"));
    assert!(r.laptop.log().unacked().await.unwrap().is_empty());
    // Repeating the reservation (a resend after a restart) adds nothing.
    r.laptop.append(&reservation(1)).await.unwrap();
    assert_eq!(r.replica.head().await.seq, 1);
}

#[tokio::test]
async fn nothing_that_releases_a_signature_is_written_while_the_replica_is_away() {
    let r = rig();
    r.laptop.append(&reservation(1)).await.unwrap();
    r.wire.set_down(true);
    assert!(r.laptop.append(&reservation(2)).await.is_err());
    assert!(r
        .laptop
        .record(kinds::SIGNED, "lease1", Some("{}"))
        .await
        .is_err());
    assert!(r
        .laptop
        .record(kinds::CLEARANCE, "0xaa", None)
        .await
        .is_err());
    // None of them is in the local log: nothing piles up behind a signature that did not go out.
    assert_eq!(r.laptop.log().head().await.unwrap().0, 1);
    assert!(matches!(r.laptop.state(), State::Unreachable(_)));
}

#[tokio::test]
async fn plain_status_is_kept_locally_and_delivered_before_the_next_release() {
    let r = rig();
    r.laptop.append(&reservation(1)).await.unwrap();
    r.wire.set_down(true);
    // A delivery and a rollback cannot be delivered, but they are not lost.
    assert!(r
        .laptop
        .record(kinds::DELIVERED, "lease1", None)
        .await
        .is_err());
    assert!(r
        .laptop
        .record(kinds::ROLLED_BACK, "lease1", None)
        .await
        .is_err());
    assert_eq!(r.laptop.log().unacked().await.unwrap().len(), 2);
    assert_eq!(r.replica.head().await.seq, 1);
    r.wire.set_down(false);
    // The backlog goes out first, in order, and then the new reservation.
    r.laptop.append(&reservation(2)).await.unwrap();
    let kinds_held: Vec<_> = r
        .replica
        .from(1, 10)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert_eq!(
        kinds_held,
        ["reservation", "delivered", "rolled_back", "reservation"]
    );
    // The rolled-back reservation lost its request proof on both machines.
    let held = r.replica.from(1, 1).await.unwrap().remove(0);
    assert!(held.payload.is_none());
    assert!(r.laptop.log().from(1, 1).await.unwrap()[0]
        .payload
        .is_none());
}

#[tokio::test]
async fn a_replica_that_lost_its_volume_is_filled_from_the_laptop_before_anything_new() {
    let r = rig();
    for n in 1..=3 {
        r.laptop.append(&reservation(n)).await.unwrap();
    }
    // The replica comes back empty, with the same key.
    let fresh = Arc::new(ReplicaNode::new(log(), key(1)));
    r.wire.swap(fresh.clone());
    r.wire.set_down(true);
    // While the link is down nothing is signed.
    assert!(r.laptop.append(&reservation(4)).await.is_err());
    r.wire.set_down(false);
    r.laptop.append(&reservation(4)).await.unwrap();
    assert_eq!(fresh.head().await.seq, 4);
    fresh.verify().await.unwrap();
    assert_eq!(r.laptop.state(), State::Ready);
}

/// A laptop restored from an old backup meets a replica that is empty (the second machine was
/// lost too). Without the hold the stale log looks like the one that is ahead and is pushed;
/// with the hold the machine stays in recovery, pushes nothing and signs nothing.
#[tokio::test]
async fn a_stale_laptop_on_hold_does_not_seed_an_empty_replica() {
    let r = rig();
    for n in 1..=3 {
        r.laptop.append(&reservation(n)).await.unwrap();
    }
    // The replica is replaced by an empty one, and a person puts the laptop on hold.
    let fresh = Arc::new(ReplicaNode::new(log(), key(1)));
    r.wire.swap(fresh.clone());
    r.laptop
        .log()
        .store()
        .run(|c| nymira_operator::store::set_recovery_hold(c, 7_200))
        .await
        .unwrap();
    assert!(r.laptop.append(&reservation(4)).await.is_err());
    assert!(r.laptop.in_recovery());
    assert_eq!(fresh.head().await.seq, 0, "nothing was pushed");
    // Lifting the hold lets the laptop fill the replica as it always did.
    r.laptop
        .log()
        .store()
        .run(|c| nymira_operator::store::clear_recovery_hold(c))
        .await
        .unwrap();
    r.laptop.append(&reservation(4)).await.unwrap();
    assert_eq!(fresh.head().await.seq, 4);
    assert!(!r.laptop.in_recovery());
}

#[tokio::test]
async fn an_acknowledgement_from_a_node_without_the_pinned_key_is_not_one() {
    let r = rig();
    r.wire.swap(Arc::new(ReplicaNode::new(log(), key(2))));
    assert!(r.laptop.append(&reservation(1)).await.is_err());
    assert!(matches!(r.laptop.state(), State::Unreachable(_)));
    assert_eq!(r.laptop.log().head().await.unwrap().0, 0);
}

#[tokio::test]
async fn a_replica_with_another_history_stops_everything() {
    let r = rig();
    r.laptop.append(&reservation(1)).await.unwrap();
    r.laptop.append(&reservation(2)).await.unwrap();
    // Another replica that holds a different second entry.
    let other = Arc::new(ReplicaNode::new(log(), key(1)));
    let history = log();
    history
        .append(
            kinds::RESERVATION,
            "lease1",
            7_200,
            r.replica.from(1, 1).await.unwrap()[0].payload.as_deref(),
            false,
        )
        .await
        .unwrap();
    history
        .append(kinds::RESERVATION, "intruder", 7_200, Some("{}"), false)
        .await
        .unwrap();
    for e in history.from(1, 10).await.unwrap() {
        other.append(&e).await.unwrap();
    }
    r.wire.swap(other);
    assert!(r.laptop.append(&reservation(3)).await.is_err());
    assert_eq!(r.laptop.state(), State::Diverged { seq: 2 });
}

#[tokio::test]
async fn a_laptop_with_an_old_disk_is_in_recovery_until_it_has_restored_from_the_replica() {
    let r = rig();
    for n in 1..=3 {
        r.laptop.append(&reservation(n)).await.unwrap();
    }
    r.laptop
        .record(kinds::DELIVERED, "lease2", None)
        .await
        .unwrap();
    // A new laptop with an empty disk, the same replica.
    let wire: Arc<dyn Link> = r.wire.clone();
    let reborn = Replicator::new(
        log(),
        wire,
        key(1).verifying_key().to_bytes(),
        Arc::new(ManualClock::new(7_200_000)),
    );
    assert!(reborn.append(&reservation(9)).await.is_err());
    assert!(reborn.in_recovery());
    assert_eq!(
        reborn.state(),
        State::Recovery {
            replica: 4,
            local: 0
        }
    );
    // Restore brings the entries back, oldest first, and ends recovery.
    let restored = reborn.restore().await.unwrap();
    assert_eq!(restored.len(), 4);
    assert_eq!(restored[3].kind, "delivered");
    assert!(!reborn.in_recovery());
    reborn.append(&reservation(9)).await.unwrap();
    assert_eq!(r.replica.head().await.seq, 5);
}

#[tokio::test]
async fn the_replica_speaks_http_and_the_laptop_checks_every_answer() {
    use nymira_operator::replica_http::{router, HttpLink};
    let replica = Arc::new(ReplicaNode::new(log(), key(1)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(replica.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let laptop = Replicator::new(
        log(),
        Arc::new(HttpLink::new(&url).unwrap()),
        key(1).verifying_key().to_bytes(),
        Arc::new(ManualClock::new(7_200_000)),
    );
    for n in 1..=3 {
        laptop.append(&reservation(n)).await.unwrap();
    }
    assert_eq!(replica.head().await.seq, 3);
    // A replica that is lost and replaced by an empty one is filled over the same channel.
    let again = Replicator::new(
        log(),
        Arc::new(HttpLink::new(&url).unwrap()),
        key(1).verifying_key().to_bytes(),
        Arc::new(ManualClock::new(7_200_000)),
    );
    assert!(again.append(&reservation(4)).await.is_err());
    assert!(again.in_recovery());
    assert_eq!(again.restore().await.unwrap().len(), 3);
    again.append(&reservation(4)).await.unwrap();
    assert_eq!(replica.head().await.seq, 4);
    // A pinned key that is not the replica's makes every answer worthless.
    let wrong = Replicator::new(
        log(),
        Arc::new(HttpLink::new(&url).unwrap()),
        key(9).verifying_key().to_bytes(),
        Arc::new(ManualClock::new(7_200_000)),
    );
    assert!(wrong.append(&reservation(5)).await.is_err());
    server.abort();
    // And with the server gone, nothing is released.
    assert!(laptop.append(&reservation(6)).await.is_err());
}

// ---------------------------------------------------------------- the operator signs its entries

#[tokio::test]
async fn a_replica_that_pins_the_operators_key_takes_only_signed_entries() {
    let operator = key(9);
    let replica = Arc::new(
        ReplicaNode::new(log(), key(1)).with_laptop_key(operator.verifying_key().to_bytes()),
    );
    let wire = Wire::new(replica.clone());
    let pk = key(1).verifying_key().to_bytes();
    let clock = || Arc::new(ManualClock::new(7_200_000));

    // The operator's own replicator signs: its entries are taken.
    let laptop = Replicator::new(log(), wire.clone(), pk, clock()).with_signing_key(operator);
    laptop.append(&reservation(1)).await.unwrap();
    laptop
        .record(kinds::SIGNED, "lease1", Some("{}"))
        .await
        .unwrap();
    assert_eq!(replica.head().await.seq, 2);

    // A peer the network admits cannot add a fact: not unsigned, not under another key.
    let head = replica.head().await;
    let forged = |sig: Option<String>| Entry {
        seq: head.seq + 1,
        prev: head.head.clone(),
        kind: kinds::SETTLED.into(),
        key: "lease1".into(),
        hour: 7_200,
        payload_hash: String::new(),
        payload: None,
        hash: String::new(),
        sig,
    };
    let mut unsigned = forged(None);
    unsigned.payload_hash = nymira_operator::replog::Log::payload_hash_of(None);
    unsigned.hash = nymira_operator::replog::Log::hash_of(&unsigned);
    assert!(replica.append(&unsigned).await.is_err());
    let stranger = nymira_operator::replog::sign_entry(&key(7), &unsigned);
    assert!(replica.append(&stranger).await.is_err());
    assert_eq!(replica.head().await.seq, 2, "nothing was added");

    // A laptop without the signing key is refused, and says so as an unreachable replica.
    let plain = Replicator::new(log(), wire, pk, clock());
    assert!(plain.append(&reservation(5)).await.is_err());
}

/// Over HTTP, a replica that pins the operator's key also refuses to read out entries or hashes
/// to a peer that does not sign the request: the entries hold request proofs and signed states.
#[tokio::test]
async fn a_pinned_replica_reads_out_only_to_a_signed_request() {
    use nymira_operator::replica_http::{router, HttpLink};
    let operator = key(9);
    let replica = Arc::new(
        ReplicaNode::new(log(), key(1)).with_laptop_key(operator.verifying_key().to_bytes()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(replica.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pk = key(1).verifying_key().to_bytes();
    let clock = || Arc::new(ManualClock::new(7_200_000));
    let signed = Replicator::new(
        log(),
        Arc::new(
            HttpLink::new(&url)
                .unwrap()
                .with_signing_key(operator.clone()),
        ),
        pk,
        clock(),
    )
    .with_signing_key(operator);
    signed.append(&reservation(1)).await.unwrap();
    // The signing laptop can read back (a restore path).
    let link = HttpLink::new(&url).unwrap();
    let http = reqwest::Client::new();
    let unsigned = http
        .get(format!("{url}/replica/entries?from=1&limit=10"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401, "an unsigned read must be refused");
    let unsigned = http
        .post(format!("{url}/replica/hashes"))
        .json(&serde_json::json!({"seqs": [1]}))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);
    // The head stays public (the replica's key signed it), and an unsigned link cannot fetch.
    assert!(Link::head(&link).await.is_ok());
    assert!(Link::fetch(&link, 1, 10).await.is_err());
    server.abort();
}
