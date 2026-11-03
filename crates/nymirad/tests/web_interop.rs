//! What the web app builds for a close and an exit, executed by a real sandbox vault.
//!
//! `web/test/vault-driver.test.ts` is run by this test (it needs `npm ci` and `npm run build:wasm`
//! in `web/`, see `web/README.md`). It creates two notes the way the app does, this test deposits
//! them into a sandbox vault, the driver builds `mutual_close` and `request_exit` calldata with the
//! app's own code and WASM, and this test sends that calldata to the vault. It also checks that
//! the app's statement words are the ones `nymirad` computes (`gateway::close::words`).
//!
//! Not covered: the note worker, the wallet connector and the screens (the driver calls the same
//! functions without them), and the operator's `POST /v1/close` route.

#[path = "support/stack.rs"]
mod stack;

use std::path::{Path, PathBuf};
use std::process::Command;

use near_workspaces::types::Gas;
use nymirad::gateway::close::words;
use nymirad::rpc::Transport;
use serde_json::{json, Value};
use stack::*;
use zkapi_types::WithdrawalPublicInputsV2C;

fn web_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web")
}

fn drive(dir: &Path, spec: Value) {
    let job = dir.join("job.json");
    std::fs::write(&job, spec.to_string()).unwrap();
    let out = Command::new("npx")
        .args(["vitest", "run", "test/vault-driver.test.ts"])
        .current_dir(web_dir())
        .env("NYMIRA_VAULT_JOB", &job)
        .output()
        .expect("npx runs (Node and `npm ci` in web/ are needed)");
    assert!(
        out.status.success(),
        "the web driver failed (is web/src/wasm built? `npm run build:wasm`):\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read(dir: &Path, name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sandbox_vault_executes_the_web_apps_close_and_exit() {
    let s = build(Transport::Direct, false).await;
    let dir = tempfile::tempdir().unwrap();

    // The web app makes two notes and shows their commitments.
    drive(
        dir.path(),
        json!({ "phase": "generate", "dir": dir.path() }),
    );
    let commitments = read(dir.path(), "commitments.json");

    // Each is deposited the way the app funds one (`ft_transfer_call` from the payout account).
    for kind in ["close", "exit"] {
        let msg = json!({ "commitment": commitments[kind] }).to_string();
        s.usdc
            .ft_transfer_call(&s.alice, s.vault.id(), ONE + FEE, &msg)
            .await
            .unwrap();
    }
    let records: Value = s
        .vault
        .view("note_records")
        .args_json(json!({ "from": 0, "limit": 2 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    // Deposited in this order, so note 0 is the close and note 1 the exit.
    let rec = |kind: &str| records[usize::from(kind == "exit")].clone();
    sync_root(&s.worker, &s.alice, &s.vault).await;

    let config: Value = s.vault.view("config").await.unwrap().json().unwrap();
    let chain_now = nymira_testkit::clock::now_seconds(&s.worker).await;
    let note = |kind: &str| {
        let r = rec(kind);
        json!({ "noteId": r["note_id"], "amount": r["deposit_amount"], "expiry": r["expiry"] })
    };
    drive(
        dir.path(),
        json!({
            "phase": "prove", "dir": dir.path(),
            "vault": s.vault.id(), "payout": s.alice.id(),
            "chainId": config["chain_id"], "operatorPk": config["operator_pk"],
            "clearancePk": config["clearance_pk"],
            "maxBound": config["bound_classes"].as_array().unwrap().last().unwrap(),
            "chainNow": chain_now,
            "notes": { "close": note("close"), "exit": note("exit") },
        }),
    );
    let out = read(dir.path(), "calldata.json");
    assert_eq!(out["gas"], "100000000000000", "the app attaches 100 Tgas");

    // The words the app builds are the ones nymirad builds from the same statement.
    for kind in ["close", "exit"] {
        let public: WithdrawalPublicInputsV2C =
            serde_json::from_str(out[kind]["publicInputs"].as_str().unwrap()).unwrap();
        let rust = words(&public);
        assert_eq!(rust.len(), 16);
        let calldata: Vec<&str> = out[kind]["args"]["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert_eq!(
            calldata, rust,
            "{kind}: the app's inputs are nymirad's words"
        );
        let hex: Vec<&str> = out[kind]["wordsHex"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        let rust_hex: Vec<String> = public
            .to_field_elements()
            .iter()
            .map(|f| hex::encode(f.as_bytes()))
            .collect();
        assert_eq!(
            hex, rust_hex,
            "{kind}: withdrawal_words is the field-element encoding"
        );
    }

    // Close: the vault takes the app's `mutual_close` and pays the payout account.
    let gas = Gas::from_gas(100_000_000_000_000);
    let before = s.usdc.balance_of(s.alice.id()).await;
    let close_commitment = commitments["close"].as_str().unwrap().to_string();
    assert_eq!(status_of(&s, &close_commitment).await, "Active");
    s.alice
        .call(s.vault.id(), "mutual_close")
        .args_json(&out["close"]["args"])
        .gas(gas)
        .transact()
        .await
        .unwrap()
        .into_result()
        .expect("the vault accepts the web-built mutual_close");
    s.worker.fast_forward(5).await.unwrap();
    assert_eq!(status_of(&s, &close_commitment).await, "Closed");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);

    // Exit: the vault takes the app's `request_exit` and lists the request, conceding nothing.
    let exit_commitment = commitments["exit"].as_str().unwrap().to_string();
    s.alice
        .call(s.vault.id(), "request_exit")
        .args_json(&out["exit"]["args"])
        .gas(gas)
        .transact()
        .await
        .unwrap()
        .into_result()
        .expect("the vault accepts the web-built request_exit");
    assert_eq!(status_of(&s, &exit_commitment).await, "PendingExit");
    let rows: Value = s
        .vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    let exit_id = rec("exit")["note_id"].clone();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["note_id"] == exit_id)
        .expect("the exit request is listed");
    assert_eq!(row["conceded"], "0");
}
