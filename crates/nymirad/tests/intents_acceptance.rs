//! Funding acceptance at the RPC and encrypted-storage boundaries.
#[path = "support/socks.rs"]
mod socks;

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
    outcome: Arc<Mutex<Value>>,
    status: Arc<Mutex<Value>>,
    requests: Arc<Mutex<Vec<Value>>>,
    base: String,
    dir: tempfile::TempDir,
}
async fn fixture(note_status: NoteStatus, success: bool, expiry: u64) -> Fixture {
    let config = Arc::new(Mutex::new(
        json!({"token": "usdc.near", "denominations": ["1000000"], "note_fee": "50000", "tvl_cap": "100000000", "current_tvl": "0"}),
    ));
    let commitment = note(note_status).commitment.clone();
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
    let outcome = Arc::new(Mutex::new(outcome));
    let edited_outcome = outcome.clone();
    let status = Arc::new(Mutex::new(
        json!({"status": "SUCCESS", "swapDetails": {"nearTxHashes": ["destination"]}}),
    ));
    let edited_status = status.clone();
    let requests = Arc::new(Mutex::new(vec![]));
    let seen = requests.clone();
    let handler = move |uri: axum::http::Uri,
                        State(config): State<Arc<Mutex<Value>>>,
                        Json(body): Json<Value>| {
        seen.lock()
            .unwrap()
            .push(json!({"path": uri.path(), "body": body}));
        let outcome = edited_outcome.lock().unwrap().clone();
        async move {
            let result = if body["method"] == "EXPERIMENTAL_tx_status" {
                if uri.path() == "/" && config.lock().unwrap()["regular_missing"] == true {
                    Value::Null
                } else {
                    outcome
                }
            } else {
                match body["params"]["request_type"].as_str() {
                    Some("view_account") => json!({"code_hash": "hash"}),
                    Some("view_access_key_list") => json!({"keys": []}),
                    _ => {
                        let data = match body["params"]["method_name"].as_str() {
                            Some("config") => config.lock().unwrap().clone(),
                            Some("pending_payout") => config
                                .lock()
                                .unwrap()
                                .get("pending")
                                .cloned()
                                .unwrap_or(json!("1050000")),
                            Some("note_records") => {
                                json!([{"note_id": 7, "deposit_amount": "1000000", "expiry": expiry}])
                            }
                            _ => json!({}),
                        };
                        json!({"result": serde_json::to_vec(&data).unwrap()})
                    }
                }
            };
            Json(json!({"jsonrpc": "2.0", "id": body["id"], "result": result}))
        }
    };
    let app = Router::new()
        .route("/", post(handler.clone()))
        .route("/archive", post(handler))
        .route(
            "/v0/status",
            get(move || {
                let status = edited_status.lock().unwrap().clone();
                async move { Json(status) }
            }),
        )
        .with_state(config.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let rpc = Rpc::new(&base, &Transport::Direct).unwrap();
    let pins = read_live(&rpc, "vault.near").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let notes = NoteStore::open(&dir.path().join("notes.sealed"), "pass", 10).unwrap();
    notes.add(note(note_status)).unwrap();
    let (_, public) = backup::generate_identity();
    let funding = Funding {
        mode: Mode::Mainnet,
        home: dir.path().into(),
        rpc,
        archival: None,
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
        phase: Default::default(),
        destination: None,
        bookings: vec![],
        notice: None,
    };
    Fixture {
        funding,
        client: FundingClient::mock(&base, Mode::Mainnet, true).unwrap(),
        attempt,
        config,
        outcome,
        status,
        requests,
        base,
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
                        phase: Default::default(),
                        destination: None,
                        bookings: vec![],
                        notice: None,
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

#[tokio::test]
async fn refused_delivery_explains_the_payout_and_claim_action_without_accepting_a_note() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let event = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "1050000", "reason": "intents_excess",
        "commitment": null, "note_id": null
    }});
    f.attempt.request = json!({"amount": "1050000"});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
        json!([format!("EVENT_JSON:{event}")]);
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(
        report.contains("claim-payout")
            && report.contains("1050000")
            && report.contains("alice.near"),
        "{report}"
    );
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
    assert!(!f.dir.path().join("calendar").exists());
}

#[tokio::test]
async fn a_restart_resumes_the_saved_destination_without_asking_oneclick_again() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.outcome.lock().unwrap()["final_execution_status"] = "EXECUTED_OPTIMISTIC".into();
    assert!(f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .is_none());
    let reopened = NoteStore::open(&f.dir.path().join("notes.sealed"), "pass", 10).unwrap();
    let mut saved = reopened
        .funding_attempts()
        .unwrap()
        .pop()
        .expect("destination phase must be durable before RPC acceptance");
    let record = serde_json::to_value(&saved).unwrap();
    assert_eq!(record["phase"], "receipt_pending");
    assert_eq!(record["destination"]["hash"], "destination");
    *f.status.lock().unwrap() = json!({"status": "FAILED"});
    f.outcome.lock().unwrap()["final_execution_status"] = "FINAL".into();
    assert_eq!(
        f.funding
            .progress(&f.client, &mut saved)
            .await
            .unwrap()
            .unwrap()
            .note_id,
        7
    );
    assert_eq!(
        serde_json::to_value(reopened.funding_attempts().unwrap().pop().unwrap()).unwrap()["phase"],
        "accepted"
    );
}

#[tokio::test]
async fn an_accepted_note_also_reports_its_excess_pending_payout() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let event = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "100000", "reason": "intents_excess",
        "commitment": f.attempt.commitment, "note_id": 7
    }});
    f.attempt.request = json!({"amount": "1150000"});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"]
        .as_array_mut()
        .unwrap()
        .push(format!("EVENT_JSON:{event}").into());
    let accepted = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .unwrap();
    assert!(
        accepted
            .warnings
            .iter()
            .any(|s| s.contains("100000") && s.contains("intents_excess")),
        "{:?}",
        accepted.warnings
    );
    assert!(accepted
        .warnings
        .iter()
        .any(|s| s.contains("claim-payout") && s.contains("account total")));
}

#[tokio::test]
async fn treasury_bookings_explain_dust_and_an_unreadable_message() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let dust = json!({"standard": "nymira", "event": "dust", "data": {
        "amount": "49999", "reason": "below_note_fee", "note_id": 7,
        "commitment": f.attempt.commitment
    }});
    f.attempt.request = json!({"amount": "1099999"});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"]
        .as_array_mut()
        .unwrap()
        .push(format!("EVENT_JSON:{dust}").into());
    let accepted = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .unwrap();
    assert!(
        accepted
            .warnings
            .iter()
            .any(|s| s.contains("49999") && s.contains("Dust") && s.contains("treasury")),
        "{:?}",
        accepted.warnings
    );

    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let unclaimed = json!({"standard": "nymira", "event": "unclaimed", "data": {
        "amount": "1050000", "reason": "intents_bad_msg", "commitment": null
    }});
    f.attempt.request = json!({"amount": "1050000"});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
        json!([format!("EVENT_JSON:{unclaimed}")]);
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(
        report.contains("could not read the message or found no valid payout account")
            && report.contains("1050000")
            && report.contains("treasury"),
        "{report}"
    );
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
    assert!(!f.dir.path().join("calendar").exists());
}

#[tokio::test]
async fn an_aged_receipt_and_the_pending_account_balance_are_read_over_socks() {
    use base64::Engine;
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.config.lock().unwrap()["regular_missing"] = true.into();
    let proxy = socks::FakeSocks::start(socks::Behavior::Tunnel(
        f.base.trim_start_matches("http://").parse().unwrap(),
    ))
    .await;
    let transport = Transport::Socks(proxy.addr.to_string());
    f.funding.rpc = Rpc::new("http://regular.invalid/", &transport).unwrap();
    f.funding.archival = Some(Rpc::new("http://archive.invalid/archive", &transport).unwrap());
    let accepted = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.note_id, 7);
    assert!(
        accepted
            .warnings
            .iter()
            .any(|s| s.contains("1050000") && s.contains("account total")),
        "{:?}",
        accepted.warnings
    );
    let seen = proxy.seen();
    assert!(seen.iter().any(|s| s.host == "regular.invalid"));
    assert!(seen.iter().any(|s| s.host == "archive.invalid"));
    let requests = f.requests.lock().unwrap();
    assert!(requests
        .iter()
        .any(|r| r["path"] == "/archive" && r["body"]["method"] == "EXPERIMENTAL_tx_status"));
    let pending = requests
        .iter()
        .find(|r| r["body"]["params"]["method_name"] == "pending_payout")
        .expect("pending account view");
    let args: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(pending["body"]["params"]["args_base64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(args, json!({"account_id": "alice.near"}));
}

#[tokio::test]
async fn a_short_delivery_matches_the_actual_settled_amount_instead_of_the_quote() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.attempt.request = json!({"amount": "1050000"});
    f.status.lock().unwrap()["swapDetails"]["amountOut"] = "500000".into();
    let event = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "500000", "reason": "intents_excess",
        "commitment": null
    }});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
        json!([format!("EVENT_JSON:{event}")]);
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(report.contains("Receipt booked payout: 500000"), "{report}");
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
}

#[tokio::test]
async fn a_delayed_or_screened_route_points_to_direct_near_usdc_without_redepositing() {
    for status in ["PROCESSING", "FAILED", "REFUNDED"] {
        let mut f = fixture(NoteStatus::Pending, true, 300).await;
        *f.status.lock().unwrap() = json!({"status": status, "swapDetails": {"refundReason": "screening blocked delivery"}});
        let result = f.funding.progress(&f.client, &mut f.attempt).await;
        let message = match result {
            Ok(None) => serde_json::to_value(&f.attempt).unwrap()["notice"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            Err(message) => message,
            other => panic!("unexpected result: {other:?}"),
        };
        assert!(
            message.contains("screening")
                && message.contains("NEAR USDC directly")
                && message.contains("do not pay twice"),
            "{status}: {message}"
        );
        assert_eq!(
            f.funding.notes.list().unwrap()[0].status,
            NoteStatus::Pending
        );
    }
}

#[tokio::test]
async fn a_failed_deposit_cannot_borrow_success_from_another_vault_receipt() {
    let mut f = fixture(NoteStatus::Pending, false, 300).await;
    let event = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "50000", "reason": "intents_excess", "commitment": f.attempt.commitment
    }});
    {
        let mut outcome = f.outcome.lock().unwrap();
        outcome["transaction_outcome"]["outcome"]["receipt_ids"] = json!(["other", "receipt"]);
        outcome["receipts_outcome"].as_array_mut().unwrap().insert(0, json!({"id": "other", "outcome": {
        "executor_id": "vault.near", "logs": [format!("EVENT_JSON:{event}")], "receipt_ids": [], "status": {"SuccessValue": ""}
    }}));
    }
    let result = f.funding.progress(&f.client, &mut f.attempt).await;
    assert!(
        result.is_err(),
        "a failed deposit log must never use another receipt's success"
    );
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
}

#[tokio::test]
async fn unrelated_malformed_failed_and_ambiguous_bookings_are_never_attributed() {
    let valid = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "1050000", "reason": "intents_excess", "commitment": null
    }});
    for (field, value) in [
        ("account", json!("other.near")),
        ("amount", json!("1050001")),
        ("amount", json!(1050000)),
        ("amount", json!("+1050000")),
        ("amount", json!("340282366920938463463374607431768211456")),
        ("commitment", json!("0xother")),
        ("commitment", json!(7)),
        ("reason", json!("unknown")),
    ] {
        let mut f = fixture(NoteStatus::Pending, true, 300).await;
        f.attempt.request = json!({"amount": "1050000"});
        let mut event = valid.clone();
        event["data"][field] = value;
        f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
            json!([format!("EVENT_JSON:{event}")]);
        assert!(f
            .funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap_err()
            .contains("no unique matching"));
        assert!(f.attempt.bookings.is_empty());
        assert_eq!(
            f.funding.notes.list().unwrap()[0].status,
            NoteStatus::Pending
        );
    }
    for (executor, success, copies) in [
        ("token.near", true, 1),
        ("vault.near", false, 1),
        ("vault.near", true, 2),
    ] {
        let mut f = fixture(NoteStatus::Pending, success, 300).await;
        f.attempt.request = json!({"amount": "1050000"});
        f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["executor_id"] =
            executor.into();
        f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
            json!(vec![format!("EVENT_JSON:{valid}"); copies]);
        let report = f
            .funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap_err();
        assert!(
            report.contains(if copies == 2 {
                "ambiguous"
            } else {
                "no unique matching"
            }),
            "{report}"
        );
        assert_eq!(
            f.funding.notes.list().unwrap()[0].status,
            NoteStatus::Pending
        );
    }
}

#[tokio::test]
async fn the_whole_final_receipt_tree_is_required_before_any_booking_or_acceptance() {
    for incomplete in ["missing_child", "not_final", "duplicate_receipt"] {
        let mut f = fixture(NoteStatus::Pending, true, 300).await;
        {
            let mut outcome = f.outcome.lock().unwrap();
            match incomplete {
                "missing_child" => {
                    outcome["receipts_outcome"][0]["outcome"]["receipt_ids"] =
                        json!(["not_returned"])
                }
                "not_final" => outcome["final_execution_status"] = "EXECUTED_OPTIMISTIC".into(),
                _ => {
                    let receipt = outcome["receipts_outcome"][0].clone();
                    outcome["receipts_outcome"]
                        .as_array_mut()
                        .unwrap()
                        .push(receipt);
                }
            }
        }
        assert!(f
            .funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap()
            .is_none());
        assert!(f.attempt.bookings.is_empty());
        assert_eq!(
            f.funding.notes.list().unwrap()[0].status,
            NoteStatus::Pending
        );
    }
}

#[test]
fn funding_records_written_before_phases_were_added_still_load() {
    let attempt: Attempt = serde_json::from_value(json!({
        "commitment": "old", "quote": {"quote": {"depositAddress": "old-address"}},
        "request": {"amount": "1050000"}, "receipt": null
    }))
    .unwrap();
    assert_eq!(attempt.phase, nymirad::intents::Phase::AddressReady);
    assert!(attempt.destination.is_none() && attempt.bookings.is_empty());
}

#[tokio::test]
async fn restoring_an_older_state_backup_keeps_the_newer_funding_phase() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let (private, public) = backup::generate_identity();
    f.funding.backup = backup::Target::new(
        backup::parse_recipient(&public).unwrap(),
        vec![f.dir.path().join("backup")],
    );
    let older = backup::StateBackup {
        version: backup::VERSION,
        taken_at: 1,
        notes: f.funding.notes.list().unwrap(),
        funding: vec![f.attempt.clone()],
        ..Default::default()
    };
    backup::write_backups(
        &f.funding.backup,
        &older.to_bytes().unwrap(),
        backup::STATE_FILE,
    )
    .unwrap();
    f.outcome.lock().unwrap()["final_execution_status"] = "EXECUTED_OPTIMISTIC".into();
    assert!(f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .is_none());
    let saved = backup::load(&[f.dir.path().join("backup")], &private).unwrap();
    assert_eq!(
        saved.funding[0].phase,
        nymirad::intents::Phase::ReceiptPending
    );
    assert_eq!(
        saved.funding[0].destination.as_ref().unwrap().hash,
        "destination"
    );
}

#[tokio::test]
async fn an_unavailable_receipt_still_checks_the_account_balance_without_confirming_a_note() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.config.lock().unwrap()["regular_missing"] = true.into();
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(
        report.contains("archival_rpc_url")
            && report.contains("account total")
            && report.contains("1050000"),
        "{report}"
    );
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
}

#[tokio::test]
async fn supported_payout_reasons_and_the_dust_boundary_keep_base_units_as_strings() {
    for (kind, reason, amount, expected) in [
        (
            "payout_pending",
            "below_note_fee",
            "49999",
            "Receipt booked payout: 49999",
        ),
        (
            "payout_pending",
            "transfer_failed",
            "1050000",
            "Receipt booked payout: 1050000",
        ),
        ("dust", "below_note_fee", "49999", "Dust: 49999"),
        ("dust", "below_note_fee", "50000", "no unique matching"),
    ] {
        let mut f = fixture(NoteStatus::Pending, true, 300).await;
        let event = json!({"standard": "nymira", "event": kind, "data": {
            "account": "alice.near", "amount": amount, "reason": reason, "commitment": f.attempt.commitment
        }});
        f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
            json!([format!("EVENT_JSON:{event}")]);
        let report = f
            .funding
            .progress(&f.client, &mut f.attempt)
            .await
            .unwrap_err();
        assert!(report.contains(expected), "{kind}/{amount}: {report}");
        assert_eq!(
            f.funding.notes.list().unwrap()[0].status,
            NoteStatus::Pending
        );
    }
}

#[tokio::test]
async fn recovery_with_dead_tor_never_falls_back_to_a_direct_archival_endpoint() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    let (tor, _state) = nymirad::tor::Tor::for_tests(
        "127.0.0.1:9".parse().unwrap(),
        nymirad::tor::TorState::Failed("no circuit".into()),
    );
    f.funding.rpc = Rpc::new(&f.base, &Transport::Tor(Arc::new(tor))).unwrap();
    f.funding.archival = Some(Rpc::new(format!("{}/archive", f.base), &Transport::Direct).unwrap());
    let before = f.requests.lock().unwrap().len();
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(report.contains("Tor is down"), "{report}");
    assert_eq!(f.requests.lock().unwrap().len(), before);
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Pending
    );
}

#[tokio::test]
async fn a_claimed_payout_is_reported_as_history_without_a_new_claim_action() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.config.lock().unwrap()["pending"] = "0".into();
    f.attempt.request = json!({"amount": "1050000"});
    let event = json!({"standard": "nymira", "event": "payout_pending", "data": {
        "account": "alice.near", "amount": "1050000", "reason": "intents_excess", "commitment": null
    }});
    f.outcome.lock().unwrap()["receipts_outcome"][0]["outcome"]["logs"] =
        json!([format!("EVENT_JSON:{event}")]);
    let report = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap_err();
    assert!(
        report.contains("1050000") && report.contains("nothing to claim"),
        "{report}"
    );
    assert!(!report.contains("Run nymirad claim-payout"), "{report}");
}

#[tokio::test]
async fn a_journal_failure_after_note_acceptance_is_a_warning_not_a_failed_deposit() {
    let mut f = fixture(NoteStatus::Pending, true, 300).await;
    f.attempt.destination = Some(
        serde_json::from_value(json!({"hash": "destination", "signer": "intents.near"})).unwrap(),
    );
    std::fs::create_dir(f.dir.path().join("notes.sealed.funding")).unwrap();
    let accepted = f
        .funding
        .progress(&f.client, &mut f.attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.note_id, 7);
    assert_eq!(
        f.funding.notes.list().unwrap()[0].status,
        NoteStatus::Accepted
    );
    assert!(
        accepted
            .warnings
            .iter()
            .any(|w| w.contains("funding journal")),
        "{:?}",
        accepted.warnings
    );
}
