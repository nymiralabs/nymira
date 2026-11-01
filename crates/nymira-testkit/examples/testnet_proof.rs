//! Proofs and commitments for the testnet smoke run of the development vault, as JSON for the
//! `near` CLI. The development keys are public, so this only works against a vault built with
//! `dev-setup`.
//!
//! `testnet_proof commitment <secret>` prints the commitment of a note with that secret.
//! `testnet_proof close <vault> <secret> <note_id> <deposit> <expiry> <payout> <valid_until>` prints
//! the `mutual_close` arguments after one charge of 20000 (a kind-1 proof of the note's state).
//! `testnet_proof escape ...` (same arguments) prints the `request_exit` arguments (kind 0).

use nymira_near_codec::Network;
use nymira_testkit::proofs::{ProofGen, WithdrawalKindId};
use serde_json::json;
use zkapi_proof::dev::{DevNote, DevRequest};
use zkapi_types::Felt252;

fn note(secret: u64, note_id: u32, deposit: u128, expiry: u64) -> DevNote {
    DevNote {
        secret: Felt252::from_u64(secret),
        note_id,
        deposit_amount: deposit,
        expiry,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["commitment", secret] => {
            let n = note(secret.parse().unwrap(), 0, 0, 0);
            let digits: String = n
                .registration()
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("0x{digits}");
        }
        [kind @ ("close" | "escape"), vault, secret, note_id, deposit, expiry, payout, valid_until] =>
        {
            let n = note(
                secret.parse().unwrap(),
                note_id.parse().unwrap(),
                deposit.parse().unwrap(),
                expiry.parse().unwrap(),
            );
            let gen = ProofGen::new(Network::Testnet, vault).unwrap();
            let s0 = n.genesis(Felt252::from_u64(7));
            let s1 = gen
                .next_state(
                    &s0,
                    &DevRequest {
                        request_time: 1_000,
                        solvency_bound: 100_000,
                        request_context: Felt252::from_u64(99),
                        rerandomization: Felt252::from_u64(9),
                    },
                    20_000,
                    &Felt252::from_u64(17),
                )
                .unwrap();
            let kind_id = if *kind == "close" {
                WithdrawalKindId::MutualClose
            } else {
                WithdrawalKindId::Escape
            };
            let bundle = gen
                .withdrawal(&s1, kind_id, payout, 0, valid_until.parse().unwrap())
                .unwrap();
            println!(
                "{}",
                json!({ "proof": bundle.proof_base64(), "inputs": bundle.inputs_base64() })
            );
        }
        _ => {
            eprintln!("usage: see the file header");
            std::process::exit(2);
        }
    }
}
