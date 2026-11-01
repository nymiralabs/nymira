//! Real zkapi proofs whose public inputs carry the NEAR mappings of
//! `nymira-near-codec` (testnet chain id, `vault.nymira.testnet`, payout account
//! `alice.testnet`), proved with the development setup:
//!
//! ```text
//! cargo run --release -p nymira-verifier --example near_fixture -- \
//!   vendor/zkapi/protocol/setup/v2-concession-dev > crates/nymira-verifier/tests/fixtures/near-fixture.json
//! ```
//!
use anyhow::{Context, Result};
use base64::Engine;
use serde::Serialize;
use zkapi_core::v2 as core;
use zkapi_proof::compact::{
    RequestProver, RequestVerifier, WithdrawalProver, WithdrawalVerifier, CIRCUIT_ID,
};
use zkapi_proof::dev::{tree_with, DevDeployment, DevNote, DevRequest, DevWithdrawal};
use zkapi_types::wire::Groth16ProofWire;
use zkapi_types::{Felt252, RequestPublicInputsV2, WithdrawalKind, WithdrawalPublicInputsV2C};

const VAULT_ACCOUNT: &str = "vault.nymira.testnet";
const PAYOUT_ACCOUNT: &str = "alice.testnet";
const TIMESTAMP: u64 = 2_000_000_000;
const VALID_UNTIL: u64 = TIMESTAMP + 10_800;

#[derive(Serialize)]
struct Fixture<T> {
    public_inputs: T,
    inputs_abi: String,
    proof_hex: String,
}

#[derive(Serialize)]
struct Fixtures {
    circuit_id: &'static str,
    setup_id: String,
    request: Fixture<RequestPublicInputsV2>,
    request_signed: Fixture<RequestPublicInputsV2>,
    withdrawal: Fixture<WithdrawalPublicInputsV2C>,
    escape_c0: Fixture<WithdrawalPublicInputsV2C>,
    escape_genesis_concession: Fixture<WithdrawalPublicInputsV2C>,
    close: Fixture<WithdrawalPublicInputsV2C>,
    ownership: Fixture<WithdrawalPublicInputsV2C>,
    escape_full_destination: Fixture<WithdrawalPublicInputsV2C>,
}

fn encode(inputs: &[Felt252], proof: &Groth16ProofWire) -> Result<(String, String)> {
    let words = inputs
        .iter()
        .flat_map(|field| field.as_bytes().iter().copied())
        .collect::<Vec<_>>();
    let proof = base64::engine::general_purpose::STANDARD
        .decode(&proof.proof)
        .context("decode proof")?;
    Ok((
        format!("0x{}", hex::encode(words)),
        format!("0x{}", hex::encode(proof)),
    ))
}

fn main() -> Result<()> {
    let setup = std::env::args()
        .nth(1)
        .context("usage: solidity_fixture SETUP_DIRECTORY")?;
    let requests = RequestProver::load(&setup)?;
    let request_verifier = RequestVerifier::load(&setup)?;
    let withdrawals = WithdrawalProver::load(&setup)?;
    let withdrawal_verifier = WithdrawalVerifier::load(&setup)?;

    let deployment = DevDeployment::new(
        nymira_near_codec::chain_id(nymira_near_codec::Network::Testnet),
        Felt252(nymira_near_codec::contract_address(VAULT_ACCOUNT)),
    );
    let payout = nymira_near_codec::destination(PAYOUT_ACCOUNT);
    let note = DevNote {
        secret: Felt252::from_u64(42),
        note_id: 0,
        deposit_amount: 1_000_000,
        expiry: 4_000_000_000,
    };
    let tree = tree_with(&[&note]);
    let s0 = note.genesis(Felt252::from_u64(7));
    let r0 = DevRequest {
        request_time: TIMESTAMP,
        solvency_bound: 500_000,
        request_context: Felt252::from_u64(99),
        rerandomization: Felt252::from_u64(9),
    };
    let s1 = s0.successor(&deployment, &r0, 120_000, &Felt252::from_u64(17))?;
    let r1 = DevRequest {
        solvency_bound: 300_000,
        request_context: Felt252::from_u64(100),
        rerandomization: Felt252::from_u64(11),
        ..r0.clone()
    };
    assert_eq!(s1.anchor, core::next_anchor(&s0.nullifier()));

    let request = |state: &zkapi_proof::dev::DevState, r: &DevRequest| -> Result<Fixture<_>> {
        let (public, witness) = state.request(&deployment, &tree, r)?;
        let proof = requests.prove(&public, witness)?;
        anyhow::ensure!(request_verifier.verify(&public, &proof)?);
        let (inputs_abi, proof_hex) = encode(&public.to_field_elements(), &proof)?;
        Ok(Fixture {
            public_inputs: public,
            inputs_abi,
            proof_hex,
        })
    };
    let withdrawal = |state: &zkapi_proof::dev::DevState,
                      kind: WithdrawalKind,
                      destination: [u8; 20],
                      conceded: u128|
     -> Result<Fixture<_>> {
        let (public, witness) = state.withdrawal(
            &deployment,
            &DevWithdrawal {
                kind,
                destination,
                conceded,
                valid_until: VALID_UNTIL,
            },
        )?;
        let proof = withdrawals.prove(&public, witness)?;
        anyhow::ensure!(withdrawal_verifier.verify(&public, &proof)?);
        let (inputs_abi, proof_hex) = encode(&public.to_field_elements(), &proof)?;
        Ok(Fixture {
            public_inputs: public,
            inputs_abi,
            proof_hex,
        })
    };

    let fixtures = Fixtures {
        circuit_id: CIRCUIT_ID,
        setup_id: requests.setup_id().to_string(),
        request: request(&s0, &r0)?,
        request_signed: request(&s1, &r1)?,
        withdrawal: withdrawal(&s1, WithdrawalKind::Escape, payout, r1.solvency_bound)?,
        escape_c0: withdrawal(&s1, WithdrawalKind::Escape, payout, 0)?,
        escape_genesis_concession: withdrawal(&s0, WithdrawalKind::Escape, payout, 250_000)?,
        close: withdrawal(&s1, WithdrawalKind::MutualClose, payout, 0)?,
        ownership: withdrawal(&s1, WithdrawalKind::Ownership, [0; 20], 0)?,
        escape_full_destination: withdrawal(&s1, WithdrawalKind::Escape, payout, 0)?,
    };
    println!("{}", serde_json::to_string_pretty(&fixtures)?);
    Ok(())
}
