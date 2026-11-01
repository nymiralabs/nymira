//! Self-test of the proof generator: NEAR-mapped request and withdrawal proofs of
//! kinds 0, 1 and 2, two separate operator keys, and the negative variants.

use std::sync::OnceLock;

use near_sdk::test_utils::VMContextBuilder;
use near_sdk::testing_env;
use nymira_near_codec::{chain_id, contract_address, destination, note_fields, Network};
use nymira_testkit::proofs::{
    withdrawal_index as w, Bundle, Kind, Layout, ProofGen, WithdrawalKindId, LAYOUT,
};
use nymira_verifier::verify;
use zkapi_core::MerkleTree;
use zkapi_proof::dev::{tree_with, DevNote, DevRequest};
use zkapi_types::Felt252;

const VAULT: &str = "vault.nymira.testnet";
const PAYOUT: &str = "alice.testnet";
const EXPIRY: u64 = 4_000_000_000;

fn gen() -> &'static ProofGen {
    static GEN: OnceLock<ProofGen> = OnceLock::new();
    GEN.get_or_init(|| ProofGen::new(Network::Testnet, VAULT).unwrap())
}

fn note(id: u32) -> DevNote {
    DevNote {
        secret: Felt252::from_u64(42 + id as u64),
        note_id: id,
        deposit_amount: 1_000_000,
        expiry: EXPIRY,
    }
}

fn request() -> DevRequest {
    DevRequest {
        request_time: 2_000_000_000,
        solvency_bound: 100_000,
        request_context: Felt252::from_u64(99),
        rerandomization: Felt252::from_u64(9),
    }
}

/// The bundle verifies through the host functions under `testing_env!`.
fn host_verifies(bundle: &Bundle, vk: &nymira_near_codec::VerifyingKey) -> bool {
    testing_env!(VMContextBuilder::new().build());
    let proof = nymira_near_codec::Proof::from_zkapi_wire(&bundle.proof_wire).unwrap();
    verify(vk, &bundle.inputs_near().unwrap(), &proof) == Ok(true)
}

#[test]
fn layout_is_the_leaf_bound_layout_of_the_pin() {
    assert_eq!(LAYOUT, Layout::LeafBound);
}

#[test]
fn request_proofs_carry_the_near_mapping_and_verify() {
    let n = note(0);
    let tree = tree_with(&[&n]);
    let s0 = n.genesis(Felt252::from_u64(7));
    let bundle = gen().request(&s0, &tree, &request()).unwrap();
    assert_eq!(bundle.inputs_wire.len(), 12);
    assert_eq!(
        &bundle.inputs_wire[1][24..],
        &chain_id(Network::Testnet).to_be_bytes()
    );
    assert_eq!(bundle.inputs_wire[2], contract_address(VAULT));
    // The active root input is the tree's actual root.
    assert_eq!(bundle.inputs_wire[3], tree.root().0);
    assert!(gen().native_verifies(&bundle, Kind::Request).unwrap());
    assert!(host_verifies(&bundle, &gen().verifying_key(Kind::Request)));
}

#[test]
fn withdrawal_proofs_of_all_three_kinds_verify() {
    let n = note(3);
    let s0 = n.genesis(Felt252::from_u64(7));
    let tree = tree_with(&[&n]);
    let s1 = gen()
        .next_state(&s0, &request(), 20_000, &Felt252::from_u64(17))
        .unwrap();
    let _ = tree;
    for (kind, conceded) in [
        (WithdrawalKindId::Escape, 10_000u128),
        (WithdrawalKindId::MutualClose, 0),
        (WithdrawalKindId::Ownership, 0),
    ] {
        let bundle = gen()
            .withdrawal(&s1, kind, PAYOUT, conceded, 2_000_010_000)
            .unwrap();
        assert_eq!(bundle.inputs_wire.len(), 16);
        assert_eq!(
            bundle.inputs_wire[w::CONTRACT_ADDRESS],
            contract_address(VAULT)
        );
        assert_eq!(
            &bundle.inputs_wire[w::CHAIN_ID][24..],
            &chain_id(Network::Testnet).to_be_bytes()
        );
        assert_eq!(
            bundle.inputs_wire[w::NOTE_FIELDS],
            note_fields(3, 1_000_000, EXPIRY)
        );
        assert_eq!(bundle.inputs_wire[w::KIND][31], kind as u8);
        let expected_destination = match kind {
            WithdrawalKindId::Ownership => [0u8; 20],
            _ => destination(PAYOUT),
        };
        assert_eq!(
            &bundle.inputs_wire[w::DESTINATION][12..],
            &expected_destination
        );
        assert!(
            gen().native_verifies(&bundle, Kind::Withdrawal).unwrap(),
            "{kind:?}"
        );
        assert!(
            host_verifies(&bundle, &gen().verifying_key(Kind::Withdrawal)),
            "{kind:?}"
        );
    }
}

#[test]
fn state_and_clearance_keys_are_separate_values() {
    let keys = gen().keys();
    assert_ne!(keys.state.x, keys.clearance.x);
    let n = note(0);
    let s0 = n.genesis(Felt252::from_u64(7));
    let bundle = gen()
        .withdrawal(&s0, WithdrawalKindId::MutualClose, PAYOUT, 0, 2_000_010_000)
        .unwrap();
    assert_eq!(bundle.inputs_wire[w::STATE_KEY_X], keys.state.x.0);
    assert_eq!(bundle.inputs_wire[w::CLEARANCE_KEY_X], keys.clearance.x.0);
}

#[test]
fn a_proof_made_with_another_clearance_key_carries_that_key() {
    let n = note(0);
    let s0 = n.genesis(Felt252::from_u64(7));
    let honest = gen()
        .withdrawal(&s0, WithdrawalKindId::MutualClose, PAYOUT, 0, 2_000_010_000)
        .unwrap();
    let wrong = gen()
        .withdrawal_with_clearance_seed(&s0, PAYOUT, 0, 2_000_010_000, 99)
        .unwrap();
    assert_ne!(
        wrong.inputs_wire[w::CLEARANCE_KEY_X],
        honest.inputs_wire[w::CLEARANCE_KEY_X]
    );
    // Still a valid proof of its own statement.
    assert!(gen().native_verifies(&wrong, Kind::Withdrawal).unwrap());
    // The state key is unchanged.
    assert_eq!(
        wrong.inputs_wire[w::STATE_KEY_X],
        honest.inputs_wire[w::STATE_KEY_X]
    );
}

#[test]
fn right_note_id_with_a_wrong_leaf_is_a_valid_proof_of_another_note() {
    let stored = note(5);
    let other = DevNote {
        secret: Felt252::from_u64(1234),
        ..note(5)
    };
    let a = gen()
        .withdrawal(
            &stored.genesis(Felt252::from_u64(7)),
            WithdrawalKindId::MutualClose,
            PAYOUT,
            0,
            2_000_010_000,
        )
        .unwrap();
    let b = gen()
        .withdrawal(
            &other.genesis(Felt252::from_u64(7)),
            WithdrawalKindId::MutualClose,
            PAYOUT,
            0,
            2_000_010_000,
        )
        .unwrap();
    assert_eq!(a.inputs_wire[w::NOTE_FIELDS], b.inputs_wire[w::NOTE_FIELDS]);
    assert_ne!(a.inputs_wire[w::NOTE_LEAF], b.inputs_wire[w::NOTE_LEAF]);
    assert!(gen().native_verifies(&b, Kind::Withdrawal).unwrap());
}

#[test]
fn forged_note_fields_and_wrong_deposit_break_the_proof() {
    let n = note(0);
    let s0 = n.genesis(Felt252::from_u64(7));
    let honest = gen()
        .withdrawal(&s0, WithdrawalKindId::MutualClose, PAYOUT, 0, 2_000_010_000)
        .unwrap();
    let vk = gen().verifying_key(Kind::Withdrawal);
    // Forged noteFields: another id packed in.
    let forged = honest.with_input(w::NOTE_FIELDS, note_fields(1, 1_000_000, EXPIRY));
    assert_eq!(
        forged.inputs_wire[w::NOTE_LEAF],
        honest.inputs_wire[w::NOTE_LEAF]
    );
    assert!(!host_verifies(&forged, &vk));
    // Right leaf, wrong deposit.
    let wrong_deposit = honest.with_input(w::NOTE_FIELDS, note_fields(0, 2_000_000, EXPIRY));
    assert!(!host_verifies(&wrong_deposit, &vk));
    assert!(!gen()
        .native_verifies(&wrong_deposit, Kind::Withdrawal)
        .unwrap());
    // The honest bundle still verifies and is untouched by the edits.
    assert!(host_verifies(&honest, &vk));
}

#[test]
fn an_old_state_gives_a_valid_proof_with_the_old_nullifier() {
    let n = note(0);
    let tree = tree_with(&[&n]);
    let s0 = n.genesis(Felt252::from_u64(7));
    let s1 = gen()
        .next_state(&s0, &request(), 10_000, &Felt252::from_u64(17))
        .unwrap();
    let old = gen().request(&s0, &tree, &request()).unwrap();
    let new = gen().request(&s1, &tree, &request()).unwrap();
    assert_ne!(old.inputs_wire[8], new.inputs_wire[8]);
    assert!(gen().native_verifies(&old, Kind::Request).unwrap());
}

#[test]
fn a_proof_against_an_old_root_is_valid_with_that_root() {
    let a = note(0);
    let b = note(1);
    let old_tree = tree_with(&[&a]);
    let new_tree = tree_with(&[&a, &b]);
    assert_ne!(old_tree.root(), new_tree.root());
    let s0 = a.genesis(Felt252::from_u64(7));
    let old = gen().request(&s0, &old_tree, &request()).unwrap();
    let new = gen().request(&s0, &new_tree, &request()).unwrap();
    assert_eq!(old.inputs_wire[3], old_tree.root().0);
    assert_eq!(new.inputs_wire[3], new_tree.root().0);
    assert!(gen().native_verifies(&old, Kind::Request).unwrap());
}

#[test]
fn a_tree_rebuilt_from_leaf_words_has_the_vault_root() {
    // The vault publishes leaf words; a client rebuilds the tree from them.
    let a = note(0);
    let b = note(1);
    let reference = tree_with(&[&a, &b]);
    let rebuilt = MerkleTree::from_leaves(&[a.leaf(), b.leaf()]);
    assert_eq!(rebuilt.root(), reference.root());
}
