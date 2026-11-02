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
