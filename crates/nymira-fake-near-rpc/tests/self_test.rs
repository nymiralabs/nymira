//! Self-test: every method and switch, through a real HTTP client.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use nymira_fake_near_rpc::*;
use serde_json::{json, Value};
use std::time::Duration;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

async fn call(url: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": "t", "method": method, "params": params});
    client()
        .post(url)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn ok(url: &str, method: &str, params: Value) -> Value {
    let r = call(url, method, params).await;
    assert!(r.get("error").is_none(), "unexpected error: {r}");
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(r["id"], "t");
    r["result"].clone()
}

async fn err(url: &str, method: &str, params: Value) -> Value {
    let r = call(url, method, params).await;
    assert!(r.get("result").is_none(), "unexpected result: {r}");
    r["error"].clone()
}

fn view(account: &str, method: &str, args: &[u8], finality: &str) -> Value {
    json!({
        "request_type": "call_function", "finality": finality, "account_id": account,
        "method_name": method, "args_base64": STANDARD.encode(args)
    })
}

fn bytes_of(result: &Value) -> Vec<u8> {
    result["result"]
        .as_array()
        .expect("result bytes")
        .iter()
        .map(|b| b.as_u64().unwrap() as u8)
        .collect()
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// A structurally valid signed transaction (the signature is not checked by the fake).
fn signed_tx(signer: &str, receiver: &str, nonce: u64) -> Vec<u8> {
    let mut tx = vec![];
    put_str(&mut tx, signer);
    tx.push(0);
    tx.extend([7u8; 32]);
    tx.extend(nonce.to_le_bytes());
    put_str(&mut tx, receiver);
    tx.extend([9u8; 32]);
    tx.extend(0u32.to_le_bytes());
    tx.push(0);
    tx.extend([5u8; 64]);
    tx
}

fn tx_hash(signed: &[u8]) -> String {
    parse_signed_tx(signed).unwrap().hash
}

#[tokio::test]
async fn call_function_matches_args_and_returns_bytes() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    rpc.set_view(
        "vault.testnet",
        "config",
        ArgsMatch::Any,
        ViewResult::json(json!({"fee": "1"})),
    );
    rpc.set_view(
        "vault.testnet",
        "note_status",
        ArgsMatch::json(json!({"note_id": "7"})),
        ViewResult::json(json!({"status": "Open"})),
    );
    rpc.set_view(
        "vault.testnet",
        "note_status",
        ArgsMatch::Any,
        ViewResult::json(json!({"status": "Unknown"})),
    );
    rpc.set_view(
        "vault.testnet",
        "raw",
        ArgsMatch::bytes(&[1, 2, 3]),
        ViewResult::bytes(&[0xff, 0x00, 0xfe]),
    );
    rpc.set_view(
        "vault.testnet",
        "boom",
        ArgsMatch::Any,
        ViewResult::error("Smart contract panicked: nope"),
    );

    let r = ok(
        &url,
        "query",
        view("vault.testnet", "config", b"{}", "final"),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&r)).unwrap(),
        json!({"fee": "1"})
    );
    assert_eq!(r["block_height"], rpc.head());
    assert!(r["block_hash"].is_string());
    assert_eq!(r["logs"], json!([]));

    // exact JSON args beat the catch-all; other args fall back to it
    let exact = ok(
        &url,
        "query",
        view(
            "vault.testnet",
            "note_status",
            br#"{ "note_id" : "7" }"#,
            "final",
        ),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&exact)).unwrap()["status"],
        "Open"
    );
    let other = ok(
        &url,
        "query",
        view(
            "vault.testnet",
            "note_status",
            br#"{"note_id":"8"}"#,
            "final",
        ),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&other)).unwrap()["status"],
        "Unknown"
    );

    // byte-exact args, junk result bytes
    let raw = ok(
        &url,
        "query",
        view("vault.testnet", "raw", &[1, 2, 3], "final"),
    )
    .await;
    assert_eq!(bytes_of(&raw), vec![0xff, 0x00, 0xfe]);
    let miss = ok(
        &url,
        "query",
        view("vault.testnet", "raw", &[1, 2], "final"),
    )
    .await;
    assert!(miss["error"].as_str().unwrap().contains("MethodNotFound"));

    // a panicking contract and an unscripted method are `error`, not a JSON-RPC error
    let boom = ok(&url, "query", view("vault.testnet", "boom", b"{}", "final")).await;
    assert_eq!(boom["error"], "Smart contract panicked: nope");
    assert!(boom.get("result").is_none());
    let none = ok(
        &url,
        "query",
        view("vault.testnet", "missing", b"{}", "final"),
    )
    .await;
    assert!(none["error"].as_str().unwrap().contains("MethodNotFound"));

    // a newer script replaces an older one
    rpc.set_view(
        "vault.testnet",
        "config",
        ArgsMatch::Any,
        ViewResult::json(json!({"fee": "2"})),
    );
    let r = ok(
        &url,
        "query",
        view("vault.testnet", "config", b"{}", "final"),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&r)).unwrap(),
        json!({"fee": "2"})
    );
}

#[tokio::test]
async fn unknown_account_has_neat_error_shape() {
    let rpc = FakeRpc::start().await;
    let e = err(
        &rpc.url(),
        "query",
        view("nobody.testnet", "config", b"{}", "final"),
    )
    .await;
    assert_eq!(e["name"], "HANDLER_ERROR");
    assert_eq!(e["cause"]["name"], "UNKNOWN_ACCOUNT");
    assert_eq!(e["cause"]["info"]["requested_account_id"], "nobody.testnet");
    assert_eq!(e["code"], -32000);
    assert!(e["data"].as_str().unwrap().contains("does not exist"));
}

#[tokio::test]
async fn accounts_and_access_keys() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    rpc.set_account(
        "operator.testnet",
        AccountSpec {
            amount: "5".into(),
            ..AccountSpec::default()
        },
    );
    rpc.set_access_keys(
        "operator.testnet",
        vec![
            AccessKeySpec::full_access("ed25519:AAA", 3),
            AccessKeySpec::function_call("ed25519:BBB", 9, "vault.testnet", &["defeat_exit"]),
        ],
    );
    let acc = ok(&url, "query", json!({"request_type": "view_account", "finality": "final", "account_id": "operator.testnet"})).await;
    assert_eq!(acc["amount"], "5");
    assert!(acc["code_hash"].is_string() && acc["block_height"].is_u64());

    let list = ok(&url, "query", json!({"request_type": "view_access_key_list", "finality": "final", "account_id": "operator.testnet"})).await;
    assert_eq!(list["keys"].as_array().unwrap().len(), 2);
    assert_eq!(list["keys"][0]["access_key"]["permission"], "FullAccess");
    assert_eq!(
        list["keys"][1]["access_key"]["permission"]["FunctionCall"]["method_names"],
        json!(["defeat_exit"])
    );

    let key = ok(&url, "query", json!({"request_type": "view_access_key", "finality": "final", "account_id": "operator.testnet", "public_key": "ed25519:BBB"})).await;
    assert_eq!(key["nonce"], 9);
    let e = err(&url, "query", json!({"request_type": "view_access_key", "finality": "final", "account_id": "operator.testnet", "public_key": "ed25519:ZZZ"})).await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_ACCESS_KEY");

    // a deleted key shows up in the next read
    rpc.advance_blocks(1, 1.0);
    rpc.set_access_keys(
        "operator.testnet",
        vec![AccessKeySpec::full_access("ed25519:AAA", 3)],
    );
    let list = ok(&url, "query", json!({"request_type": "view_access_key_list", "finality": "final", "account_id": "operator.testnet"})).await;
    assert_eq!(list["keys"].as_array().unwrap().len(), 1);

    let e = err(
        &url,
        "query",
        json!({"request_type": "view_account", "finality": "final", "account_id": "ghost.testnet"}),
    )
    .await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_ACCOUNT");
}

#[tokio::test]
async fn blocks_status_and_gas_price() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    let g = ok(&url, "block", json!({"finality": "final"})).await;
    assert_eq!(g["header"]["height"], GENESIS_HEIGHT);
    assert_eq!(g["header"]["timestamp"], GENESIS_TIME_NS);
    assert_eq!(
        g["header"]["timestamp_nanosec"],
        GENESIS_TIME_NS.to_string()
    );

    let head = rpc.advance_blocks(5, 2.0);
    assert_eq!(head, GENESIS_HEIGHT + 5);
    let b = ok(&url, "block", json!({"finality": "final"})).await;
    assert_eq!(b["header"]["height"], head);
    assert_eq!(b["header"]["timestamp"], GENESIS_TIME_NS + 10_000_000_000);

    // by height, by hash; the prev_hash chain holds
    let prev = ok(&url, "block", json!({"block_id": head - 1})).await;
    assert_eq!(b["header"]["prev_hash"], prev["header"]["hash"]);
    let by_hash = ok(&url, "block", json!({"block_id": b["header"]["hash"]})).await;
    assert_eq!(by_hash["header"]["height"], head);

    let e = err(&url, "block", json!({"block_id": head + 1})).await;
    assert_eq!(
        (e["name"].as_str(), e["cause"]["name"].as_str()),
        (Some("HANDLER_ERROR"), Some("UNKNOWN_BLOCK"))
    );
    let e = err(
        &url,
        "block",
        json!({"block_id": "11111111111111111111111111111111"}),
    )
    .await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_BLOCK");

    // explicit block: a fixed timestamp and a fixed hash, and the head follows it
    rpc.set_block(head + 100, 1_800_000_000_000_000_123);
    rpc.set_block_hash(head + 100, "FixedHashFixedHashFixedHashFixedHash11111");
    let s = ok(&url, "status", json!([])).await;
    assert_eq!(s["chain_id"], "testnet");
    assert_eq!(s["sync_info"]["latest_block_height"], head + 100);
    assert_eq!(
        s["sync_info"]["latest_block_hash"],
        "FixedHashFixedHashFixedHashFixedHash11111"
    );
    assert_eq!(
        s["sync_info"]["latest_block_time"],
        "2027-01-15T08:00:00.000000123Z"
    );
    assert_eq!(s["sync_info"]["syncing"], false);
    let gp = ok(&url, "gas_price", json!([null])).await;
    assert!(gp["gas_price"].as_str().unwrap().parse::<u64>().unwrap() > 0);

    // views report the block they were read at
    rpc.set_view("a.testnet", "m", ArgsMatch::Any, ViewResult::json(json!(1)));
    let r = ok(&url, "query", view("a.testnet", "m", b"{}", "final")).await;
    assert_eq!(r["block_height"], head + 100);
    // reading at an older block does not see an account scripted later
    let e = err(
        &url,
        "query",
        json!({"request_type": "view_account", "block_id": head, "account_id": "a.testnet"}),
    )
    .await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_ACCOUNT");
    assert_eq!(e["cause"]["info"]["block_height"], head);
}

#[tokio::test]
async fn transaction_status_with_receipts_and_events() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    let event = event_log(
        "nymira",
        "1.0.0",
        "request_exit",
        json!({"note_id": "7", "nullifier": "0xab"}),
    );
    rpc.set_tx_status(
        TxStatusSpec::success("H1", "alice.testnet", "vault.testnet")
            .receipt(ReceiptSpec::new("vault.testnet").log(event.clone()))
            .receipt(
                ReceiptSpec::new("usdc.testnet")
                    .status(OutcomeSpec::Success(STANDARD.encode("true"))),
            ),
    );
    rpc.set_tx_status(
        TxStatusSpec::success("H2", "alice.testnet", "vault.testnet")
            .status(OutcomeSpec::Failure("Smart contract panicked: bad".into())),
    );
    for method in ["tx", "EXPERIMENTAL_tx_status"] {
        let t = ok(&url, method, json!(["H1", "alice.testnet"])).await;
        assert_eq!(t["final_execution_status"], "FINAL");
        assert_eq!(t["transaction"]["hash"], "H1");
        let outs = t["receipts_outcome"].as_array().unwrap();
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0]["outcome"]["logs"][0], event);
        assert_eq!(outs[0]["outcome"]["executor_id"], "vault.testnet");
        assert_eq!(
            outs[0]["outcome"]["status"],
            json!({"SuccessReceiptId": outs[1]["id"]})
        );
        assert_eq!(
            outs[1]["outcome"]["status"]["SuccessValue"],
            STANDARD.encode("true")
        );
        assert_eq!(t["receipts"].as_array().unwrap().len(), 2);
    }
    // object params
    let t = ok(
        &url,
        "tx",
        json!({"tx_hash": "H1", "sender_account_id": "alice.testnet", "wait_until": "FINAL"}),
    )
    .await;
    assert_eq!(t["transaction"]["signer_id"], "alice.testnet");

    let bad = ok(&url, "tx", json!(["H2", "alice.testnet"])).await;
    assert!(
        bad["status"]["Failure"]["ActionError"]["kind"]["FunctionCallError"]["ExecutionError"]
            .is_string()
    );

    let e = err(&url, "tx", json!(["NOPE", "alice.testnet"])).await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_TRANSACTION");
    assert_eq!(e["cause"]["info"]["requested_transaction_hash"], "NOPE");
}

#[tokio::test]
async fn submissions_are_recorded_and_queryable() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    let a = signed_tx("op.testnet", "vault.testnet", 11);
    let b = signed_tx("op.testnet", "vault.testnet", 12);
    let c = signed_tx("op.testnet", "vault.testnet", 13);

    let r = ok(
        &url,
        "send_tx",
        json!({"signed_tx_base64": STANDARD.encode(&a), "wait_until": "FINAL"}),
    )
    .await;
    assert_eq!(r["transaction"]["hash"], tx_hash(&a));
    assert_eq!(r["transaction"]["nonce"], 11);
    let r = ok(&url, "broadcast_tx_commit", json!([STANDARD.encode(&b)])).await;
    assert_eq!(r["transaction"]["receiver_id"], "vault.testnet");
    let r = ok(&url, "broadcast_tx_async", json!([STANDARD.encode(&c)])).await;
    assert_eq!(r, json!(tx_hash(&c)));

    let subs = rpc.submissions();
    assert_eq!(subs.len(), 3);
    assert_eq!(subs[0].method, "send_tx");
    assert_eq!(subs[0].wait_until.as_deref(), Some("FINAL"));
    assert_eq!(subs[0].tx_base64, STANDARD.encode(&a));
    assert_eq!(
        (subs[1].method.as_str(), subs[1].nonce),
        ("broadcast_tx_commit", 12)
    );
    assert_eq!(subs[2].signer_id, "op.testnet");

    // a sent transaction can be looked up by the hash the client computes
    let t = ok(&url, "tx", json!([tx_hash(&a), "op.testnet"])).await;
    assert_eq!(t["status"], json!({"SuccessValue": ""}));

    // a scripted status for that hash wins over the default
    rpc.set_tx_status(
        TxStatusSpec::success(&tx_hash(&b), "op.testnet", "vault.testnet")
            .status(OutcomeSpec::Failure("nope".into())),
    );
    let t = ok(&url, "tx", json!([tx_hash(&b), "op.testnet"])).await;
    assert!(t["status"].get("Failure").is_some());

    // the control endpoint shows the same list
    let listed: Value = client()
        .get(format!("{url}/control/submissions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 3);
    assert_eq!(listed[0]["tx_hash"], tx_hash(&a));

    let e = err(&url, "send_tx", json!({"signed_tx_base64": "!!"})).await;
    assert_eq!(e["code"], -32602);
    let e = err(
        &url,
        "send_tx",
        json!({"signed_tx_base64": STANDARD.encode(b"junk")}),
    )
    .await;
    assert_eq!(e["code"], -32602);
    assert_eq!(rpc.submissions().len(), 3);
}

#[tokio::test]
async fn protocol_errors() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    let e = err(&url, "no_such_method", json!([])).await;
    assert_eq!(
        (e["code"].as_i64(), e["cause"]["name"].as_str()),
        (Some(-32601), Some("METHOD_NOT_FOUND"))
    );
    let e = err(&url, "query", json!({"finality": "final"})).await;
    assert_eq!(e["code"], -32602);
    let e = err(
        &url,
        "query",
        json!({"request_type": "view_code", "finality": "final", "account_id": "x"}),
    )
    .await;
    assert_eq!(e["code"], -32602);
    let e = err(&url, "block", json!({})).await;
    assert_eq!(e["code"], -32602);
    let e = err(&url, "tx", json!([])).await;
    assert_eq!(e["code"], -32602);

    let raw = client().post(&url).body("not json").send().await.unwrap();
    assert_eq!(raw.status(), 200);
    let v: Value = raw.json().await.unwrap();
    assert_eq!(v["error"]["code"], -32700);
}

#[tokio::test]
async fn lagging_node_serves_old_blocks_and_old_state() {
    let fresh = FakeRpc::start().await;
    let lagging = FakeRpc::start().await;
    for rpc in [&fresh, &lagging] {
        rpc.set_view(
            "v.testnet",
            "exit_requests",
            ArgsMatch::Any,
            ViewResult::json(json!([])),
        );
        rpc.advance_blocks(10, 1.0);
        rpc.set_view(
            "v.testnet",
            "exit_requests",
            ArgsMatch::Any,
            ViewResult::json(json!([{"note_id": "1"}])),
        );
        rpc.set_tx_status(TxStatusSpec::success("H", "a.testnet", "v.testnet"));
        rpc.advance_blocks(2, 1.0);
    }
    lagging.set_lag(5);

    let f = ok(&fresh.url(), "block", json!({"finality": "final"})).await;
    let l = ok(&lagging.url(), "block", json!({"finality": "final"})).await;
    assert_eq!(f["header"]["height"], GENESIS_HEIGHT + 12);
    assert_eq!(l["header"]["height"], GENESIS_HEIGHT + 7);
    assert!(l["header"]["timestamp"].as_u64() < f["header"]["timestamp"].as_u64());
    assert_eq!(
        ok(&lagging.url(), "status", json!([])).await["sync_info"]["latest_block_height"],
        GENESIS_HEIGHT + 7
    );

    // it cannot serve the blocks it has not got, and the same block has the same hash on both
    let e = err(
        &lagging.url(),
        "block",
        json!({"block_id": GENESIS_HEIGHT + 12}),
    )
    .await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_BLOCK");
    let on_fresh = ok(
        &fresh.url(),
        "block",
        json!({"block_id": GENESIS_HEIGHT + 7}),
    )
    .await;
    assert_eq!(on_fresh["header"]["hash"], l["header"]["hash"]);

    // state is stale too: the view written at block +10 and the transaction in it are not visible yet
    let fv = ok(
        &fresh.url(),
        "query",
        view("v.testnet", "exit_requests", b"{}", "final"),
    )
    .await;
    let lv = ok(
        &lagging.url(),
        "query",
        view("v.testnet", "exit_requests", b"{}", "final"),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&fv)).unwrap(),
        json!([{"note_id": "1"}])
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&lv)).unwrap(),
        json!([])
    );
    assert_eq!(
        err(&lagging.url(), "tx", json!(["H", "a.testnet"])).await["cause"]["name"],
        "UNKNOWN_TRANSACTION"
    );
    ok(&fresh.url(), "tx", json!(["H", "a.testnet"])).await;

    // catching up
    lagging.set_lag(0);
    let l = ok(&lagging.url(), "block", json!({"finality": "final"})).await;
    assert_eq!(l["header"]["height"], GENESIS_HEIGHT + 12);
}

#[tokio::test]
async fn fork_changes_hashes_from_a_height() {
    let main = FakeRpc::start().await;
    let forked = FakeRpc::start().await;
    for rpc in [&main, &forked] {
        rpc.advance_blocks(10, 1.0);
    }
    let fork_at = GENESIS_HEIGHT + 6;
    forked.set_fork(Some(ForkSpec {
        from_height: fork_at,
        tag: "evil".into(),
    }));

    let hash = |rpc: &FakeRpc, h: u64| {
        let url = rpc.url();
        async move { ok(&url, "block", json!({"block_id": h})).await["header"]["hash"].clone() }
    };
    for h in GENESIS_HEIGHT..fork_at {
        assert_eq!(
            hash(&main, h).await,
            hash(&forked, h).await,
            "common history at {h}"
        );
    }
    for h in fork_at..=GENESIS_HEIGHT + 10 {
        assert_ne!(hash(&main, h).await, hash(&forked, h).await, "fork at {h}");
    }
    // same height, same timestamp, different hash, and the fork's chain is self-consistent
    let a = ok(&main.url(), "block", json!({"finality": "final"})).await;
    let b = ok(&forked.url(), "block", json!({"finality": "final"})).await;
    assert_eq!(a["header"]["height"], b["header"]["height"]);
    assert_eq!(a["header"]["timestamp"], b["header"]["timestamp"]);
    assert_ne!(a["header"]["hash"], b["header"]["hash"]);
    let prev = ok(
        &forked.url(),
        "block",
        json!({"block_id": GENESIS_HEIGHT + 9}),
    )
    .await;
    assert_eq!(b["header"]["prev_hash"], prev["header"]["hash"]);
    // the forked hash resolves on the fork only
    let e = err(
        &main.url(),
        "block",
        json!({"block_id": b["header"]["hash"]}),
    )
    .await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_BLOCK");

    forked.set_fork(None);
    assert_eq!(hash(&main, fork_at).await, hash(&forked, fork_at).await);
}

#[tokio::test]
async fn views_can_differ_between_instances() {
    let honest = FakeRpc::start().await;
    let liar = FakeRpc::start().await;
    honest.set_view(
        "v.testnet",
        "pending_payout",
        ArgsMatch::Any,
        ViewResult::json(json!("100")),
    );
    liar.set_view(
        "v.testnet",
        "pending_payout",
        ArgsMatch::Any,
        ViewResult::json(json!("999")),
    );
    let h = ok(
        &honest.url(),
        "query",
        view("v.testnet", "pending_payout", b"{}", "final"),
    )
    .await;
    let l = ok(
        &liar.url(),
        "query",
        view("v.testnet", "pending_payout", b"{}", "final"),
    )
    .await;
    assert_ne!(bytes_of(&h), bytes_of(&l));
    assert_eq!(h["block_hash"], l["block_hash"]);
}

#[tokio::test]
async fn failure_modes() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    rpc.set_view(
        "v.testnet",
        "config",
        ArgsMatch::Any,
        ViewResult::json(json!({})),
    );

    rpc.set_failure(Failure::Http(503));
    let r = client()
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "status", "params": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);

    rpc.set_failure(Failure::RpcError {
        code: -32000,
        message: "Server error".into(),
    });
    let e = err(&url, "status", json!([])).await;
    assert_eq!(
        (e["code"].as_i64(), e["message"].as_str()),
        (Some(-32000), Some("Server error"))
    );

    rpc.set_failure(Failure::Garbage);
    let r = client()
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "status", "params": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.json::<Value>().await.is_err());

    rpc.set_failure(Failure::Timeout);
    let e = client()
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "status", "params": []}))
        .timeout(Duration::from_millis(300))
        .send()
        .await
        .unwrap_err();
    assert!(e.is_timeout());

    // the attempts were all counted, and the control endpoint still answers
    assert_eq!(rpc.requests().len(), 4);
    let state: Value = client()
        .get(format!("{url}/control/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["failure"], "timeout");

    rpc.set_failure(Failure::None);
    ok(&url, "status", json!([])).await;

    // latency delays the answer
    rpc.set_latency(Duration::from_millis(300));
    let t = std::time::Instant::now();
    ok(&url, "status", json!([])).await;
    assert!(t.elapsed() >= Duration::from_millis(300));
    rpc.set_latency(Duration::ZERO);
    let t = std::time::Instant::now();
    ok(&url, "status", json!([])).await;
    assert!(t.elapsed() < Duration::from_millis(250));
}

#[tokio::test]
async fn requests_are_logged_and_reset_clears_state() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    rpc.set_view(
        "v.testnet",
        "config",
        ArgsMatch::Any,
        ViewResult::json(json!({})),
    );
    rpc.set_lag(3);
    rpc.advance_blocks(5, 1.0);
    ok(
        &url,
        "query",
        view("v.testnet", "note_status", b"{}", "final"),
    )
    .await;
    let log = rpc.requests();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].method, "query");
    assert_eq!(log[0].params["method_name"], "note_status");

    rpc.reset();
    assert!(rpc.requests().is_empty());
    let e = err(&url, "query", view("v.testnet", "config", b"{}", "final")).await;
    assert_eq!(e["cause"]["name"], "UNKNOWN_ACCOUNT");
    let s = ok(&url, "status", json!([])).await;
    assert_eq!(
        s["sync_info"]["latest_block_height"],
        GENESIS_HEIGHT + 5,
        "lag cleared, blocks kept"
    );
}

#[tokio::test]
async fn control_endpoint_scripts_everything() {
    let rpc = FakeRpc::start().await;
    let url = rpc.url();
    let c = client();
    let post = |body: Value| {
        let (c, url) = (c.clone(), url.clone());
        async move {
            c.post(format!("{url}/control"))
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let cmds = [
        json!({"cmd": "set_view", "account": "v.testnet", "method": "config", "args": "any", "result": {"json": {"fee": "3"}}}),
        json!({"cmd": "set_view", "account": "v.testnet", "method": "note_status", "args": {"json": {"note_id": "1"}}, "result": {"base64": STANDARD.encode("raw")}}),
        json!({"cmd": "set_view", "account": "v.testnet", "method": "bad", "result": {"error": "panic"}}),
        json!({"cmd": "set_account", "account": "op.testnet", "spec": {"amount": "42"}}),
        json!({"cmd": "set_access_keys", "account": "op.testnet", "keys": [{"public_key": "ed25519:K"}]}),
        json!({"cmd": "advance_blocks", "n": 3, "seconds_per_block": 1.5}),
        json!({"cmd": "set_block_hash", "height": GENESIS_HEIGHT + 3, "hash": "Pinned"}),
        json!({"cmd": "set_tx_status", "status": {"tx_hash": "T", "signer_id": "a.testnet", "receiver_id": "v.testnet",
            "receipts": [{"executor_id": "v.testnet", "logs": ["hello"]}]}}),
        json!({"cmd": "set_latency_ms", "ms": 0}),
        json!({"cmd": "set_lag", "blocks": 0}),
        json!({"cmd": "set_fork", "fork": null}),
        json!({"cmd": "set_failure", "failure": "none"}),
    ];
    for cmd in cmds {
        let r = post(cmd.clone()).await;
        assert_eq!(r.status(), 200, "{cmd}");
    }
    assert_eq!(rpc.head(), GENESIS_HEIGHT + 3);

    let r = ok(&url, "query", view("v.testnet", "config", b"{}", "final")).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes_of(&r)).unwrap(),
        json!({"fee": "3"})
    );
    let r = ok(
        &url,
        "query",
        view("v.testnet", "note_status", br#"{"note_id":"1"}"#, "final"),
    )
    .await;
    assert_eq!(bytes_of(&r), b"raw");
    assert_eq!(
        ok(&url, "query", view("v.testnet", "bad", b"{}", "final")).await["error"],
        "panic"
    );
    let acc = ok(
        &url,
        "query",
        json!({"request_type": "view_account", "finality": "final", "account_id": "op.testnet"}),
    )
    .await;
    assert_eq!(acc["amount"], "42");
    let keys = ok(&url, "query", json!({"request_type": "view_access_key_list", "finality": "final", "account_id": "op.testnet"})).await;
    assert_eq!(keys["keys"][0]["access_key"]["permission"], "FullAccess");
    let b = ok(&url, "block", json!({"finality": "final"})).await;
    assert_eq!(b["header"]["hash"], "Pinned");
    assert_eq!(b["header"]["timestamp"], GENESIS_TIME_NS + 4_500_000_000);
    let t = ok(&url, "tx", json!(["T", "a.testnet"])).await;
    assert_eq!(t["receipts_outcome"][0]["outcome"]["logs"][0], "hello");

    // switches through the endpoint
    assert_eq!(
        post(json!({"cmd": "set_lag", "blocks": 2})).await.status(),
        200
    );
    assert_eq!(
        ok(&url, "status", json!([])).await["sync_info"]["latest_block_height"],
        GENESIS_HEIGHT + 1
    );
    assert_eq!(
        post(json!({"cmd": "set_failure", "failure": {"http": 502}}))
            .await
            .status(),
        200
    );
    let r = c
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "status", "params": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(
        post(json!({"cmd": "set_fork", "fork": {"from_height": GENESIS_HEIGHT, "tag": "x"}}))
            .await
            .status(),
        200
    );
    assert_eq!(post(json!({"cmd": "reset"})).await.status(), 200);
    assert_eq!(
        ok(&url, "status", json!([])).await["sync_info"]["latest_block_height"],
        GENESIS_HEIGHT + 3
    );

    // malformed commands are refused, not applied
    assert_eq!(post(json!({"cmd": "set_view", "account": "a", "method": "m", "args": {"base64": "!!"}, "result": {"json": 1}})).await.status(), 400);
    assert_eq!(
        post(json!({"cmd": "advance_blocks", "n": 1, "seconds_per_block": -1.0}))
            .await
            .status(),
        400
    );
    assert_eq!(
        post(json!({"cmd": "set_failure", "failure": {"http": 5}}))
            .await
            .status(),
        400
    );
    assert!(post(json!({"cmd": "nope"}))
        .await
        .status()
        .is_client_error());
}

/// Three providers behind one client: a client that compares them can tell which is which.
#[tokio::test]
async fn three_providers_are_distinguishable() {
    let honest = FakeRpc::start().await;
    let lagging = FakeRpc::start().await;
    let forked = FakeRpc::start().await;
    let failing = FakeRpc::start().await;
    for rpc in [&honest, &lagging, &forked, &failing] {
        rpc.set_view(
            "v.testnet",
            "config",
            ArgsMatch::Any,
            ViewResult::json(json!({"epoch": 1})),
        );
        rpc.advance_blocks(20, 1.0);
    }
    lagging.set_lag(4);
    forked.set_fork(Some(ForkSpec {
        from_height: GENESIS_HEIGHT + 15,
        tag: "f".into(),
    }));
    failing.set_failure(Failure::Http(500));

    #[derive(Debug, PartialEq)]
    enum Seen {
        Ok { height: u64, hash: String },
        Down,
    }
    let probe = |url: String| async move {
        let r = client().post(&url).json(&json!({"jsonrpc": "2.0", "id": 1, "method": "block", "params": {"finality": "final"}})).send().await;
        match r {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) if v.get("result").is_some() => Seen::Ok {
                    height: v["result"]["header"]["height"].as_u64().unwrap(),
                    hash: v["result"]["header"]["hash"].as_str().unwrap().to_string(),
                },
                _ => Seen::Down,
            },
            _ => Seen::Down,
        }
    };
    let h = probe(honest.url()).await;
    let l = probe(lagging.url()).await;
    let f = probe(forked.url()).await;
    let x = probe(failing.url()).await;
    let (
        Seen::Ok {
            height: hh,
            hash: hash_h,
        },
        Seen::Ok {
            height: lh,
            hash: hash_l,
        },
        Seen::Ok {
            height: fh,
            hash: hash_f,
        },
    ) = (&h, &l, &f)
    else {
        panic!("honest, lagging and forked all answer: {h:?} {l:?} {f:?}");
    };
    assert_eq!(x, Seen::Down);
    // lagging: lower height; forked: same height as honest, different hash
    assert!(lh < hh);
    assert_eq!(fh, hh);
    assert_ne!(hash_f, hash_h);
    assert_ne!(hash_l, hash_h);
    // and the lagging node's block is the honest chain's block at that height
    let same = ok(&honest.url(), "block", json!({"block_id": lh})).await;
    assert_eq!(same["header"]["hash"].as_str().unwrap(), hash_l);
}

#[tokio::test]
async fn binary_prints_its_address_and_serves() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let exe = env!("CARGO_BIN_EXE_nymira-fake-near-rpc");
    let mut child = Command::new(exe)
        .args(["--addr", "127.0.0.1:0", "--chain-id", "binary-test"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line
        .trim()
        .strip_prefix("listening on ")
        .expect("address line")
        .to_string();
    let s = ok(&url, "status", json!([])).await;
    assert_eq!(s["chain_id"], "binary-test");
    let r = client()
        .post(format!("{url}/control"))
        .json(&json!({"cmd": "set_view", "account": "v.testnet", "method": "m", "result": {"json": 7}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = ok(&url, "query", view("v.testnet", "m", b"{}", "final")).await;
    assert_eq!(bytes_of(&r), b"7");
    child.kill().unwrap();
    child.wait().unwrap();
}
