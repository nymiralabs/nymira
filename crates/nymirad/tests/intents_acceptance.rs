//! Funding acceptance at the RPC and encrypted-storage boundaries.
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use nymira_operator::clock::ManualClock;
use nymirad::{
    backup,
    intents::{Attempt, Funding},
    mode::Mode,
    oneclick::FundingClient,
    pins::read_live,
    rpc::{Rpc, Transport},
    secrets::{NoteEntry, NoteStatus, NoteStore},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn note(status: NoteStatus) -> NoteEntry {
    NoteEntry {
        chain_label: "near-mainnet".into(),
        secret: "00".repeat(32),
        commitment: format!("0x{}", "01".repeat(32)),
        denomination: "1000000".into(),
        payout: "alice.near".into(),
        status,
        note_id: None,
        expiry_ns: None,
        deposit_amount: None,
        tx_hash: None,
        created_at: 1,
    }
}
struct Fixture {
    funding: Funding,
    client: FundingClient,
    attempt: Attempt,
    config: Arc<Mutex<Value>>,
    dir: tempfile::TempDir,
}
async fn fixture(status: NoteStatus, success: bool, expiry: u64) -> Fixture {
    let config = Arc::new(Mutex::new(
        json!({"token": "usdc.near", "denominations": ["1000000"], "note_fee": "50000", "tvl_cap": "100000000", "current_tvl": "0"}),
    ));
    let commitment = note(status).commitment.clone();
    let log = format!(
        "EVENT_JSON:{}",
        json!({"standard": "nymira", "event": "deposit", "data": {"commitment": commitment, "note_id": 7, "expiry": expiry, "denomination": "1000000", "withdraw_to": "alice.near"}})
    );
    let receipt_status = if success {
        json!({"SuccessValue": ""})
    } else {
        json!({"Failure": {"ActionError": "rolled back"}})
    };
    let outcome = json!({"final_execution_status": "FINAL", "transaction_outcome": {"outcome": {"receipt_ids": ["receipt"]}}, "receipts_outcome": [{"id": "receipt", "outcome": {"executor_id": "vault.near", "logs": [log], "receipt_ids": [], "status": receipt_status}}]});
    let app = Router::new().route("/", post(move |State(config): State<Arc<Mutex<Value>>>, Json(body): Json<Value>| {
        let outcome = outcome.clone();
        async move {
            let result = if body["method"] == "EXPERIMENTAL_tx_status" { outcome } else {
                match body["params"]["request_type"].as_str() {
                    Some("view_account") => json!({"code_hash": "hash"}),
                    Some("view_access_key_list") => json!({"keys": []}),
                    _ => {
                        let data = match body["params"]["method_name"].as_str() {
                            Some("config") => config.lock().unwrap().clone(),
                            Some("note_records") => json!([{"note_id": 7, "deposit_amount": "1000000", "expiry": expiry}]),
                            _ => json!({}),
                        };
                        json!({"result": serde_json::to_vec(&data).unwrap()})
                    }
                }
            };
            Json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        }
    })).route("/v0/status", get(|| async { Json(json!({"status": "SUCCESS", "swapDetails": {"nearTxHashes": ["destination"]}})) })).with_state(config.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let rpc = Rpc::new(&base, &Transport::Direct).unwrap();
    let pins = read_live(&rpc, "vault.near").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let notes = NoteStore::open(&dir.path().join("notes.sealed"), "pass", 10).unwrap();
    notes.add(note(status)).unwrap();
    let (_, public) = backup::generate_identity();
    let funding = Funding {
        mode: Mode::Mainnet,
        home: dir.path().into(),
        rpc,
        pins,
        notes,
        backup: backup::Target::new(
            backup::parse_recipient(&public).unwrap(),
            vec![dir.path().join("backup")],
        ),
        clock: Arc::new(ManualClock::new(200_000)),
    };
    let attempt = Attempt {
        commitment,
        request: json!({}),
        quote: json!({"quote": {"depositAddress": "fake"}}),
        receipt: None,
    };
    Fixture {
        funding,
        client: FundingClient::mock(&base, Mode::Mainnet, true).unwrap(),
        attempt,
        config,
        dir,
    }
}
#[tokio::test]
async fn a_failed_receipts_deposit_log_cannot_accept_a_note() {
    let mut f = fixture(NoteStatus::Pending, false, 300).await;
    assert!(f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err()
        .contains("receipt failed"));
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
    assert!(!f.dir.path().join("calendar").exists());
}
#[tokio::test]
async fn recovery_preserves_closed_and_exiting_notes() {
    for status in [NoteStatus::Closed, NoteStatus::Exiting] {
        let mut f = fixture(status, true, 300).await;
        assert!(f
            .funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap_err()
            .contains("status is unchanged"));
        assert_eq!(f.funding.notes.list().unwrap()[0].status, status);
    }
}
#[tokio::test]
async fn an_expired_pending_deposit_can_be_recovered_for_exit() {
    let mut f = fixture(NoteStatus::Pending, true, 99).await;
    assert_eq!(
        f.funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap()
            .unwrap()
            .note_id,
        7
    );
    assert_eq!(f.funding.notes.list().unwrap()[0].expiry_ns, Some(99));
}
#[tokio::test]
async fn full_cap_and_changed_pins_stop_before_quotes() {
    let f = fixture(NoteStatus::Pending, true, 300).await;
    f.config.lock().unwrap()["current_tvl"] = "100000000".into();
    assert!(f
        .funding
        .check(1_000_000)
        .await
        .unwrap_err()
        .contains("TVL cap"));
    f.config.lock().unwrap()["note_fee"] = "60000".into();
    assert!(f
        .funding
        .check(1_000_000)
        .await
        .unwrap_err()
        .contains("pins differ"));
}
#[test]
fn concurrent_writers_keep_every_note_and_encrypted_funding_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.sealed");
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let path = path.clone();
            std::thread::spawn(move || {
                let store = NoteStore::open(&path, "pass", 10).unwrap();
                let mut n = note(NoteStatus::Pending);
                n.commitment = format!("commitment-{i}");
                store.add(n.clone()).unwrap();
                store
                    .save_funding(&Attempt {
                        commitment: n.commitment.clone(),
                        request: json!({"refundTo": "private-source-wallet"}),
                        quote: json!({}),
                        receipt: None,
                    })
                    .unwrap();
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let store = NoteStore::open(&path, "pass", 10).unwrap();
    assert_eq!(store.list().unwrap().len(), 8);
    assert_eq!(store.funding_attempts().unwrap().len(), 8);
    for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
        let bytes = std::fs::read(file.path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private-source-wallet"));
    }
}

#[tokio::test]
async fn payout_rejects_reserved_accounts_and_malformed_names() {
    let f = fixture(NoteStatus::Pending, true, 300).await;
    let config = f.config.lock().unwrap().clone();
    for payout in [
        "vault.near",
        "usdc.near",
        "intents.near",
        "a_-b.near",
        "-a.near",
        "A.near",
    ] {
        assert!(f
            .funding
            .payout(payout, &config)
            .await
            .unwrap_err()
            .contains("invalid payout"));
    }
}
