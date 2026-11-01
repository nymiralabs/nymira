//! An unchanged `zkapi_client::Wallet` makes the request proof; the operator answers it.

mod common;

use common::*;
use nymira_near_codec::chain_id;
use nymira_operator::clock::ManualClock;
use zkapi_client::config::{ClientConfig, ClientProofMode};
use zkapi_client::wallet::Wallet;
use zkapi_core::v2 as core;
use zkapi_proof::compact::CompactSigner;
use zkapi_types::wire::OpenRouterLeaseStatusResponse;
use zkapi_types::{canonical_payload_hash, Felt252, MERKLE_DEPTH};

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_wallet_proof_opens_a_lease_and_the_wallet_ignores_the_token() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        clock_secs: now,
        ..Setup::default()
    })
    .await;
    let state_dir = tempfile::tempdir().unwrap();
    let mut wallet = Wallet::new(ClientConfig {
        protocol_version: 2,
        chain_id: chain_id(network()),
        contract_address: contract(),
        request_charge_cap: 100_000,
        policy_charge_cap: 100_000,
        policy_enabled: false,
        server_url: rig.url.clone(),
        state_dir: state_dir.path().to_string_lossy().into_owned(),
        proof_mode: ClientProofMode::Groth16 {
            setup_dir: SETUP_DIR.into(),
        },
        state_signing_key: CompactSigner::from_seed(&Felt252::from_u64(1)).public_key(),
        clearance_signing_key: CompactSigner::from_seed(&Felt252::from_u64(2)).public_key(),
        settlement_window_seconds: 21_905,
    })
    .unwrap();
    wallet
        .confirm_deposit(Felt252::from_u64(42), 0, 2_000_000, EXPIRY)
        .unwrap();
    let siblings = core::zero_hashes()[..MERKLE_DEPTH].to_vec();
    let payload = payload();
    let request = wallet
        .prepare_request_with_bound(
            &payload,
            canonical_payload_hash(payload.as_bytes()),
            tree().root(),
            siblings,
            100_000,
        )
        .unwrap();

    let (status, body, _) = rig.lease(&request).await;
    assert_eq!(status, 201, "{body}");
    // Wire compatibility: zkapi's own lease type parses the body and ignores the extra fields.
    let parsed: OpenRouterLeaseStatusResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.status, "active");
    assert_eq!(parsed.client_request_id, request.client_request_id);
    assert!(body["lease_token"].is_string());
    assert!(body.get("api_key").is_none());
    let _ = ManualClock::new(0);
}
