//! The replication log and the replica node: what the operator relies on for "a state is on the
//! second machine before it is released".

use ed25519_dalek::SigningKey;
use nymira_operator::replog::{audit, kinds, verify_head, Audit, Log, LogError, ReplicaNode};
use nymira_operator::store::Store;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn log() -> Log {
    Log::new(Store::open_in_memory().unwrap())
}

fn node(seed: u8) -> ReplicaNode {
    ReplicaNode::new(log(), key(seed))
}

/// Three reservations on a laptop log, in order.
async fn laptop(n: usize) -> Log {
    let laptop = log();
    for i in 0..n {
        laptop
            .append(
                kinds::RESERVATION,
                &format!("lease{i}"),
                7_200 + i as u64,
                Some("{}"),
                false,
            )
            .await
            .unwrap();
    }
    laptop
}

#[tokio::test]
async fn an_acknowledgement_is_signed_by_the_replica_key_and_by_nobody_else() {
    let laptop = laptop(1).await;
    let replica = node(1);
    let entry = laptop.from(1, 10).await.unwrap().remove(0);
    let ack = replica.append(&entry).await.unwrap();
    assert_eq!((ack.seq, ack.head.as_str()), (1, entry.hash.as_str()));
    assert!(verify_head(&key(1).verifying_key().to_bytes(), &ack));
    // A node without the key cannot make an acknowledgement the laptop accepts.
    let forged = node(2).append(&entry).await.unwrap();
    assert!(!verify_head(&key(1).verifying_key().to_bytes(), &forged));
    // The signature covers the numbers: changing one breaks it.
    let mut moved = ack.clone();
    moved.seq = 2;
    assert!(!verify_head(&key(1).verifying_key().to_bytes(), &moved));
}

#[tokio::test]
async fn a_chain_must_extend_the_head_and_a_repeat_changes_nothing() {
    let laptop = laptop(3).await;
    let entries = laptop.from(1, 10).await.unwrap();
    let replica = node(1);
    replica.append(&entries[0]).await.unwrap();
    // A gap is refused and says where the head is.
    assert_eq!(
        replica.append(&entries[2]).await.unwrap_err(),
        LogError::Gap { head: 1 }
    );
    replica.append(&entries[1]).await.unwrap();
    // The same entry again is acknowledged and the head stays.
    let again = replica.append(&entries[1]).await.unwrap();
    assert_eq!(again.seq, 2);
}

#[tokio::test]
async fn the_replica_refuses_a_chain_that_disagrees_with_the_one_it_holds() {
    let replica = node(1);
    let honest = laptop(3).await;
    for e in honest.from(1, 10).await.unwrap() {
        replica.append(&e).await.unwrap();
    }
    // Another history that is shorter, and one entry at the head's place with other content.
    let other = log();
    for i in 0..2 {
        other
            .append(
                kinds::RESERVATION,
                &format!("other{i}"),
                7_200,
                Some("{}"),
                false,
            )
            .await
            .unwrap();
    }
    let shorter = other.from(1, 10).await.unwrap();
    assert_eq!(
        replica.append(&shorter[0]).await.unwrap_err(),
        LogError::Fork { seq: 1 }
    );
    assert_eq!(replica.head().await.seq, 3);
}

#[tokio::test]
async fn an_entry_whose_payload_does_not_match_its_hash_is_refused() {
    let laptop = laptop(1).await;
    let mut entry = laptop.from(1, 10).await.unwrap().remove(0);
    entry.payload = Some("{\"changed\":true}".into());
    assert!(matches!(
        node(1).append(&entry).await.unwrap_err(),
        LogError::Bad(_)
    ));
}

#[tokio::test]
async fn the_challenge_finds_a_replica_restored_from_an_old_snapshot() {
    let laptop = laptop(5).await;
    let entries = laptop.from(1, 10).await.unwrap();
    // The replica acknowledged five entries, then was restored to the state after three.
    let restored = node(1);
    for e in &entries[..3] {
        restored.append(e).await.unwrap();
    }
    let verdict = audit(&laptop, &restored, &key(1).verifying_key().to_bytes(), 5)
        .await
        .unwrap();
    assert_eq!(
        verdict,
        Audit::Behind {
            replica: 3,
            local: 5
        }
    );

    // Caught up, the same audit passes.
    for e in &entries[3..] {
        restored.append(e).await.unwrap();
    }
    let verdict = audit(&laptop, &restored, &key(1).verifying_key().to_bytes(), 5)
        .await
        .unwrap();
    assert_eq!(verdict, Audit::Match);
}

#[tokio::test]
async fn a_replica_that_rewrote_an_old_entry_is_found_by_the_challenge() {
    let laptop = laptop(4).await;
    let replica = node(1);
    // Same length, but its second entry is not the laptop's.
    let other = log();
    other
        .append(kinds::RESERVATION, "lease0", 7_200, Some("{}"), false)
        .await
        .unwrap();
    other
        .append(kinds::RESERVATION, "forged", 7_201, Some("{}"), false)
        .await
        .unwrap();
    for e in other.from(1, 10).await.unwrap() {
        replica.append(&e).await.unwrap();
    }
    for e in laptop.from(3, 10).await.unwrap() {
        // The chain cannot continue from a forked prefix: the replica says so.
        assert!(replica.append(&e).await.is_err());
    }
    let verdict = audit(&laptop, &replica, &key(1).verifying_key().to_bytes(), 4)
        .await
        .unwrap();
    assert!(matches!(verdict, Audit::Diverged { .. }), "{verdict:?}");
}

#[tokio::test]
async fn a_payload_past_its_retention_goes_and_the_chain_stays_checkable() {
    let laptop = laptop(3).await;
    let replica = node(1);
    for e in laptop.from(1, 10).await.unwrap() {
        replica.append(&e).await.unwrap();
    }
    let head = replica.head().await;
    assert_eq!(replica.redact("lease1").await.unwrap(), 1);
    laptop.redact("lease1").await.unwrap();
    let entries = replica.from(1, 10).await.unwrap();
    assert!(entries[0].payload.is_some());
    assert!(entries[1].payload.is_none());
    // Hashes are untouched; the head is the same; both machines still verify.
    assert_eq!(replica.head().await, head);
    replica.verify().await.unwrap();
    laptop.verify().await.unwrap();
    // The redacted entry still chains: a later entry is accepted on top of it.
    laptop
        .append(kinds::SETTLED, "lease0", 7_300, None, false)
        .await
        .unwrap();
    let next = laptop.from(4, 1).await.unwrap().remove(0);
    replica.append(&next).await.unwrap();
}

#[tokio::test]
async fn the_time_in_an_entry_is_rounded_to_the_hour() {
    let laptop = log();
    let entry = laptop
        .append(
            kinds::RESERVATION,
            "lease",
            3_600 * 5 + 1_234,
            Some("{}"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(entry.hour, 3_600 * 5);
}
