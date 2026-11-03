mod common;

use common::*;
use nymira_operator::operator::felt_hex;
use nymira_operator::status::{status_message, STATUS_DOMAIN};
use serde::Deserialize;
use serde_json::json;
use zkapi_core::v2::{clearance_message, domain_value, state_message};
use zkapi_proof::compact::verify_signature;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::{Felt252, SchnorrSignature};

#[tokio::test(flavor = "multi_thread")]
async fn status_follows_the_schema() {
    let rig = Rig::new(Setup::default()).await;
    let s = rig.status().await;
    assert_eq!(s["version"], 1);
    assert_eq!(s["vault"], VAULT);
    let keys = vault_config();
    assert_eq!(s["operator_pk"]["x"], keys.operator_pk.x);
    assert_eq!(s["clearance_pk"]["y"], keys.clearance_pk.y);
    assert_eq!(s["note_fee"], "10000");
    assert_eq!(s["close_fee"], "10000");
    assert_eq!(s["bound_classes"], json!(["100000", "500000"]));
    assert_eq!(s["charge_step"], "1000");
    assert_eq!(s["min_charge"], "2000");
    assert_eq!(s["slot_rate"], 1000);
    assert_eq!(s["open_notes"], 0);
    assert_eq!(s["external_notes"], 0);
    assert_eq!(s["team_note_ids"], json!([]));
    assert_eq!(s["nonce"]["host"], "operator.test");
    assert_eq!(s["admission"]["load_pct"], 0);
    assert_eq!(s["admission"]["puzzle_difficulty"], 0);
    assert!(s["admission"]["retry_after_secs"].is_u64());
    assert_eq!(s["daily_budget"]["limit"], "250000");
    assert_eq!(s["daily_budget"]["used"], "0");
    assert_eq!(s["daily_budget"]["resets_ts"], (T0 / 86_400 + 1) * 86_400);
    assert_eq!(s["recovery_mode"], false);
    assert!(s["emergency_notice"].is_null());
    assert_eq!(s["degraded"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_counts_external_notes_from_the_team_list() {
    let rig = Rig::new(Setup {
        tune: Box::new(|c| c.team_note_ids = vec![0, 1]),
        ..Setup::default()
    })
    .await;
    rig.vault.edit(|v| {
        v.open_notes = 5;
        v.open_team_notes = 2;
    });
    rig.clock.advance_secs(2);
    let s = rig.status().await;
    assert_eq!(s["open_notes"], 5);
    assert_eq!(s["external_notes"], 3);
    assert_eq!(s["team_note_ids"], json!([0, 1]));
}

fn pk(v: &serde_json::Value) -> CurvePointWire {
    CurvePointWire {
        x: Felt252::from_hex(v["x"].as_str().unwrap()).unwrap(),
        y: Felt252::from_hex(v["y"].as_str().unwrap()).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_in_one_slot_get_the_same_signed_nonce() {
    let rig = Rig::new(Setup::default()).await;
    let (a, b) = (rig.status().await, rig.status().await);
    assert_eq!(a["nonce"], b["nonce"]);
    assert_eq!(a["nonce_sig"], b["nonce_sig"]);
    rig.clock.advance_secs(5);
    let c = rig.status().await;
    assert_ne!(
        a["nonce"]["nonce"], c["nonce"]["nonce"],
        "a new slot, a new nonce"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_nonce_signature_verifies_under_the_state_key_and_is_no_state_or_clearance_signature() {
    let rig = Rig::new(Setup::default()).await;
    let s = rig.status().await;
    let state_key = pk(&s["operator_pk"]);
    let sig: SchnorrSignature = serde_json::from_value(s["nonce_sig"].clone()).unwrap();
    let n = &s["nonce"];
    let (nonce, host, ts) = (
        n["nonce"].as_str().unwrap(),
        n["host"].as_str().unwrap(),
        n["ts"].as_u64().unwrap(),
    );
    assert_eq!(ts % 5, 0, "the slot start");
    assert!(verify_signature(&state_key, &status_message(nonce, host, ts), &sig).unwrap());
    // Another host or time is another message.
    assert!(!verify_signature(&state_key, &status_message(nonce, "other.test", ts), &sig).unwrap());
    assert!(!verify_signature(&state_key, &status_message(nonce, host, ts + 5), &sig).unwrap());
    // It cannot be read as a state or a clearance signature for any value the nonce could name.
    let word = Felt252(
        <[u8; 32]>::try_from([vec![0u8; 16], hex::decode(nonce).unwrap()].concat()).unwrap(),
    );
    let state = state_message(2, chain_id_u64(), &contract(), &word, &word, &word);
    let clearance = clearance_message(2, chain_id_u64(), &contract(), &word);
    assert!(!verify_signature(&state_key, &state, &sig).unwrap());
    assert!(!verify_signature(&state_key, &clearance, &sig).unwrap());
    assert!(!verify_signature(
        &pk(&s["clearance_pk"]),
        &status_message(nonce, host, ts),
        &sig
    )
    .unwrap());
    // Its domain tag is its own.
    let tag = domain_value(STATUS_DOMAIN);
    assert_ne!(tag, domain_value(b"zkapi.v2.state"));
    assert_ne!(tag, domain_value(b"zkapi.v2.clear"));
    let _ = felt_hex(&tag);
}

fn chain_id_u64() -> u64 {
    nymira_near_codec::chain_id(network())
}

#[derive(Deserialize)]
struct ClientView {
    version: u32,
    operator_pk: serde_json::Value,
    nonce: serde_json::Value,
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_ignores_a_field_it_does_not_know() {
    let rig = Rig::new(Setup::default()).await;
    let mut s = rig.status().await;
    s["added_in_a_later_phase"] = json!({"minter": "ok"});
    let view: ClientView = serde_json::from_value(s).unwrap();
    assert_eq!(view.version, 1);
    assert!(view.operator_pk["x"].is_string() && view.nonce["nonce"].is_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn quote_and_attestation_answer() {
    let rig = Rig::new(Setup::default()).await;
    let get = |path: &'static str| {
        let rig = &rig;
        async move {
            let r = rig
                .http
                .get(format!("{}{path}", rig.url))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200, "{path}");
            r.json::<serde_json::Value>().await.unwrap()
        }
    };
    // Exact documents: a client reads these fields, so a change must show here.
    assert_eq!(
        get("/v2/billing/quote").await,
        json!({ "status": "ok", "pricing": "usdc_1_to_1", "units_per_usd": "1000000", "quote": null })
    );
    assert_eq!(
        get("/v1/attestation").await,
        json!({ "status": "ok", "attestation": null, "mode": "none", "version": 1 })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cors_admits_the_web_app_origin_only() {
    let rig = Rig::new(Setup::default()).await;
    let preflight = |origin: &str| {
        rig.http
            .request(
                reqwest::Method::OPTIONS,
                format!("{}/v2/openrouter/leases", rig.url),
            )
            .header("origin", origin.to_string())
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "content-type,x-nymira-lease-token,x-nymira-puzzle",
            )
            .send()
    };
    let ok = preflight(ORIGIN).await.unwrap();
    assert!(ok.status().is_success());
    let h = ok.headers();
    assert_eq!(h["access-control-allow-origin"], ORIGIN);
    let allowed = h["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        allowed.contains("x-nymira-lease-token") && allowed.contains("x-nymira-puzzle"),
        "{allowed}"
    );
    assert!(h["access-control-allow-methods"]
        .to_str()
        .unwrap()
        .contains("POST"));
    assert_eq!(h["access-control-max-age"], "600");
    assert!(h.get("access-control-allow-credentials").is_none());
    for other in ["https://evil.example", "null"] {
        let r = preflight(other).await.unwrap();
        assert!(
            r.headers().get("access-control-allow-origin").is_none(),
            "{other}"
        );
    }
    // A real response carries the origin and exposes Retry-After.
    let got = rig
        .http
        .get(format!("{}/v1/status", rig.url))
        .header("origin", ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(got.headers()["access-control-allow-origin"], ORIGIN);
    assert!(got.headers()["access-control-expose-headers"]
        .to_str()
        .unwrap()
        .contains("retry-after"));
}

#[test]
fn the_published_team_note_list_loads() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/team-note-ids.json");
    assert_eq!(
        nymira_operator::operator::load_team_note_ids(std::path::Path::new(path)).unwrap(),
        Vec::<u32>::new()
    );
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("ids.json");
    std::fs::write(&other, "[3, 5]").unwrap();
    assert_eq!(
        nymira_operator::operator::load_team_note_ids(&other).unwrap(),
        vec![3, 5]
    );
    std::fs::write(&other, "{\"ids\": []}").unwrap();
    assert!(nymira_operator::operator::load_team_note_ids(&other).is_err());
}
