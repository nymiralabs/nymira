//! The key minter as a process: what the public API gets over its socket and what it cannot ask.

use std::sync::Arc;

use nymira_operator::clock::{Clock, ManualClock};
use nymira_operator::ipc;
use nymira_operator::minter_service::{serve, RemoteProvider};
use nymira_operator::nearai::LeaseChat;
use nymira_operator::provider::{FakeProvider, ProviderAdapter, ProviderError};
use nymira_operator::usage::{verify, Minter, UsageRecord, UsageSigner};

fn uid() -> u32 {
    // SAFETY: no arguments, no failure mode.
    unsafe { libc::getuid() }
}

struct Rig {
    remote: RemoteProvider,
    fake: Arc<FakeProvider>,
    minter: Minter,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

fn rig(allowed_uid: u32) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("minter.sock");
    let clock = Arc::new(ManualClock::new(1_900_000_000_000));
    let fake = Arc::new(FakeProvider::new(clock.clone(), 5_000_000));
    let minter = Minter::generate();
    tokio::spawn(serve(
        ipc::bind(&path).unwrap(),
        fake.clone(),
        minter.clone(),
        clock.clone(),
        allowed_uid,
    ));
    let chat = LeaseChat::new(reqwest::Client::new(), "http://127.0.0.1:1");
    Rig {
        remote: RemoteProvider::new(&path, chat),
        fake,
        minter,
        clock,
        _dir: dir,
    }
}

#[tokio::test]
async fn the_api_mints_reads_and_revokes_through_the_socket() {
    let r = rig(uid());
    assert_eq!(r.remote.balance().await.unwrap(), 5_000_000);
    let key = r.remote.mint("lease1", 100_000).await.unwrap();
    assert!(!key.key_id.is_empty());
    assert_eq!(r.remote.list().await.unwrap(), ["lease1"]);
    r.fake.set_usage("lease1", 42_000_000);
    assert_eq!(r.remote.usage("lease1").await.unwrap().cost_nano_usd, 42_000_000);
    r.remote.revoke("lease1").await.unwrap();
    assert!(!r.fake.is_live("lease1"));
    assert!(matches!(
        r.remote.usage("nobody").await,
        Err(ProviderError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_usage_record_is_signed_only_for_what_the_provider_reports() {
    let r = rig(uid());
    r.remote.mint("lease1", 100_000).await.unwrap();
    r.fake.set_usage("lease1", 7_000);
    let now = r.clock.now_secs();
    let record = |cost: Option<&str>, read_at| UsageRecord {
        lease_id: "lease1".into(),
        cost_nano_usd: cost.map(Into::into),
        read_at,
    };
    let signed = r.remote.sign(record(Some("7000"), now)).await.unwrap();
    assert!(verify(&r.minter.public_key(), &signed));
    // The API process cannot name a charge of its own, claim the usage was unreadable while it
    // is readable, or date a record far from the minter's clock.
    assert!(r.remote.sign(record(Some("1"), now)).await.is_err());
    assert!(r.remote.sign(record(None, now)).await.is_err());
    assert!(r.remote.sign(record(Some("7000"), now + 3_600)).await.is_err());
    // Unreadable usage can only be signed as unreadable.
    r.fake.set_usage_error(Some(ProviderError::Unavailable("down".into())));
    assert!(r.remote.sign(record(Some("7000"), now)).await.is_err());
    assert!(r.remote.sign(record(None, now)).await.is_ok());
}

#[tokio::test]
async fn the_secret_reaches_the_api_and_a_restart_asks_again() {
    let r = rig(uid());
    r.remote.mint("lease1", 100_000).await.unwrap();
    // After minting, the secret is in the API's memory; a second API process asks the minter.
    let again = RemoteProvider::new(
        &r._dir.path().join("minter.sock"),
        LeaseChat::new(reqwest::Client::new(), "http://127.0.0.1:1"),
    );
    // No gateway at the base URL: the chat call fails after it has the secret, not before.
    let err = again
        .chat(
            "lease1",
            nymira_operator::provider::ChatRequest {
                body: bytes::Bytes::from_static(b"{}"),
                headers: vec![],
            },
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ProviderError::Unavailable(_)), "{err}");
    r.remote.revoke("lease1").await.unwrap();
    // A process with no cached secret finds the key gone.
    let third = RemoteProvider::new(
        &r._dir.path().join("minter.sock"),
        LeaseChat::new(reqwest::Client::new(), "http://127.0.0.1:1"),
    );
    let err = third
        .chat(
            "lease1",
            nymira_operator::provider::ChatRequest {
                body: bytes::Bytes::from_static(b"{}"),
                headers: vec![],
            },
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ProviderError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn the_minter_drops_a_peer_of_another_user() {
    let r = rig(uid() + 1);
    assert!(r.remote.balance().await.is_err());
}
