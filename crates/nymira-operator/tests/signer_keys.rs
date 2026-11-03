//! The signing passphrases: a minimum length, and the public keys they give (what the vault's
//! `new()` takes as `operator_pk` and `clearance_pk`).

use nymira_operator::signer::{
    check_passphrase_strength, public_keys_from_passphrases, seeds_from_passphrases, Signer,
    MIN_PASSPHRASE_CHARS,
};

const STATE: &str = "a long state passphrase of words";
const CLEARANCE: &str = "another long clearance passphrase";
// A few KDF rounds: the test is about the wiring, not the cost of a guess.
const ROUNDS: u32 = 10;

#[test]
fn a_short_passphrase_is_refused_outside_the_sandbox() {
    let short = "x".repeat(MIN_PASSPHRASE_CHARS - 1);
    assert!(check_passphrase_strength(&short).is_err());
    assert!(check_passphrase_strength(&"x".repeat(MIN_PASSPHRASE_CHARS)).is_ok());
    assert!(public_keys_from_passphrases(&short, CLEARANCE, ROUNDS, false).is_err());
    assert!(public_keys_from_passphrases(STATE, &short, ROUNDS, false).is_err());
    // The sandbox profile keeps its short test passphrases.
    assert!(public_keys_from_passphrases(&short, CLEARANCE, ROUNDS, true).is_ok());
}

#[test]
fn the_printed_keys_are_the_keys_the_signer_serves() {
    let (operator_pk, clearance_pk) =
        public_keys_from_passphrases(STATE, CLEARANCE, ROUNDS, false).expect("keys");
    let (state_seed, clearance_seed) =
        seeds_from_passphrases(STATE, CLEARANCE, ROUNDS).expect("seeds");
    let (state, clearance) =
        Signer::signers_from_seeds(&state_seed, &clearance_seed, false).expect("signers");
    assert_eq!(operator_pk, state.public_key());
    assert_eq!(clearance_pk, clearance.public_key());
    assert_ne!(operator_pk, clearance_pk);
    // Equal passphrases are refused, short or not.
    assert!(public_keys_from_passphrases(STATE, STATE, ROUNDS, false).is_err());
}
