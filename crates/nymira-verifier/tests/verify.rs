//! The verifier against real zkapi development fixtures, differentially
//! against native arkworks Groth16. `alt_bn128_*` are the real host functions
//! under `testing_env!`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use ark_bn254::{Bn254, Fq, Fq2, G1Affine, G2Affine};
use ark_ec::AffineRepr;
use ark_ff::PrimeField;
use ark_groth16::{Groth16, Proof as ArkProof, VerifyingKey as ArkVk};
use ark_serialize::CanonicalDeserialize;
use ark_snark::SNARK;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::testing_env;
use nymira_near_codec::{
    public_input_from_wire, CodecError, Proof, VerifyingKey, BASE_MODULUS_BE, SCALAR_MODULUS_BE,
};
use nymira_verifier::{verify, VerifyError};
use zkapi_proof::compact::{strip_key_header, KeyKind};

struct Case {
    name: &'static str,
    kind: KeyKind,
    inputs: Vec<[u8; 32]>, // wire (big-endian) words
    proof: Vec<u8>,        // 256-byte wire proof
}

fn hex_bytes(value: &serde_json::Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn cases() -> Vec<Case> {
    let fixture: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zkapi-dev-fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    [
        ("request", KeyKind::Request),
        ("request_signed", KeyKind::Request),
        ("withdrawal", KeyKind::Withdrawal),
        ("escape_c0", KeyKind::Withdrawal),
        ("escape_genesis_concession", KeyKind::Withdrawal),
        ("close", KeyKind::Withdrawal),
        ("ownership", KeyKind::Withdrawal),
        ("escape_full_destination", KeyKind::Withdrawal),
    ]
    .into_iter()
    .map(|(name, kind)| Case {
        name,
        kind,
        inputs: hex_bytes(&fixture[name]["inputs_abi"])
            .chunks(32)
            .map(|word| word.try_into().unwrap())
            .collect(),
        proof: hex_bytes(&fixture[name]["proof_hex"]),
    })
    .collect()
}

/// The canonical verifying key bytes: the key file without its header.
fn canonical_vk(kind: KeyKind) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/zkapi/protocol/setup/v2-concession-dev")
        .join(kind.verifying_key_file());
    let bytes = std::fs::read(path).unwrap();
    strip_key_header(&bytes, kind).unwrap().1.to_vec()
}

fn near_inputs(words: &[[u8; 32]]) -> Vec<[u8; 32]> {
    words
        .iter()
        .map(|w| public_input_from_wire(w).unwrap())
        .collect()
}

fn run(vk: &VerifyingKey, inputs: &[[u8; 32]], proof: &Proof) -> Result<bool, VerifyError> {
    testing_env!(VMContextBuilder::new().build());
    verify(vk, inputs, proof)
}

/// Same verification through the host, with an abort counted as a rejection.
fn host_accepts(vk: &VerifyingKey, inputs: &[[u8; 32]], proof: &Proof) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        matches!(run(vk, inputs, proof), Ok(true))
    }))
    .unwrap_or(false)
}

fn fq(word_be: &[u8]) -> Fq {
    Fq::from_be_bytes_mod_order(word_be)
}

/// Native Groth16 over arkworks for a wire proof; `false` for any malformed encoding.
fn native_accepts(kind: KeyKind, inputs: &[[u8; 32]], wire: &[u8]) -> bool {
    let vk = ArkVk::<Bn254>::deserialize_compressed(&canonical_vk(kind)[..]).unwrap();
    if wire.len() != 256 || wire.chunks(32).any(|w| w >= &BASE_MODULUS_BE[..]) {
        return false;
    }
    let w: Vec<&[u8]> = wire.chunks(32).collect();
    let a = G1Affine::new_unchecked(fq(w[0]), fq(w[1]));
    let b = G2Affine::new_unchecked(Fq2::new(fq(w[2]), fq(w[3])), Fq2::new(fq(w[4]), fq(w[5])));
    let c = G1Affine::new_unchecked(fq(w[6]), fq(w[7]));
    let on_curve = |ok: bool| ok;
    if !(on_curve(a.is_on_curve() && !a.is_zero())
        && on_curve(
            b.is_on_curve() && b.is_in_correct_subgroup_assuming_on_curve() && !b.is_zero(),
        )
        && on_curve(c.is_on_curve() && !c.is_zero()))
    {
        return false;
    }
    let mut scalars = Vec::new();
    for input in inputs {
        if input >= &SCALAR_MODULUS_BE {
            return false;
        }
        scalars.push(ark_bn254::Fr::from_be_bytes_mod_order(input));
    }
    if scalars.len() + 1 != vk.gamma_abc_g1.len() {
        return false;
    }
    Groth16::<Bn254>::verify(&vk, &scalars, &ArkProof { a, b, c }).unwrap_or(false)
}

fn near_vk(kind: KeyKind) -> VerifyingKey {
    VerifyingKey::from_canonical(&canonical_vk(kind)).unwrap()
}

#[test]
fn fixture_proofs_verify() {
    for case in cases() {
        let proof = Proof::from_zkapi_wire(&case.proof).unwrap();
        let ok = run(&near_vk(case.kind), &near_inputs(&case.inputs), &proof);
        assert_eq!(ok, Ok(true), "{}", case.name);
        assert!(
            native_accepts(case.kind, &case.inputs, &case.proof),
            "{}",
            case.name
        );
    }
}

#[test]
fn input_counts_match_the_statements() {
    for case in cases() {
        let expected = if case.kind == KeyKind::Request {
            12
        } else {
            16
        };
        assert_eq!(case.inputs.len(), expected, "{}", case.name);
        assert_eq!(near_vk(case.kind).ic.len(), expected + 1);
    }
}

#[test]
fn tampered_inputs_are_rejected() {
    for case in cases() {
        let vk = near_vk(case.kind);
        let proof = Proof::from_zkapi_wire(&case.proof).unwrap();
        for index in 0..case.inputs.len() {
            let mut words = case.inputs.clone();
            // Add one to the last byte; skip when that would reach the modulus.
            words[index][31] = words[index][31].wrapping_add(1);
            if words[index] >= SCALAR_MODULUS_BE {
                continue;
            }
            let inputs = near_inputs(&words);
            assert_eq!(
                run(&vk, &inputs, &proof),
                Ok(false),
                "{} input {index}",
                case.name
            );
            assert!(!native_accepts(case.kind, &words, &case.proof));
        }
    }
}

#[test]
fn a_proof_under_the_other_statements_key_is_rejected() {
    let all = cases();
    let request = all.iter().find(|c| c.name == "request").unwrap();
    let withdrawal = all.iter().find(|c| c.name == "withdrawal").unwrap();
    // Request inputs and proof under the withdrawal key: wrong input count.
    let proof = Proof::from_zkapi_wire(&request.proof).unwrap();
    assert_eq!(
        run(
            &near_vk(KeyKind::Withdrawal),
            &near_inputs(&request.inputs),
            &proof
        ),
        Err(VerifyError::InputCount)
    );
    // Right count, wrong key: the pairing fails.
    let proof = Proof::from_zkapi_wire(&withdrawal.proof).unwrap();
    let mut padded = near_inputs(&withdrawal.inputs);
    padded.truncate(12);
    assert_eq!(run(&near_vk(KeyKind::Request), &padded, &proof), Ok(false));
}

#[test]
fn another_proofs_points_are_rejected() {
    let all = cases();
    let first = &all[0];
    let second = &all[1];
    // Swap A between two valid request proofs: valid points, wrong proof.
    let mut wire = first.proof.clone();
    wire[..64].copy_from_slice(&second.proof[..64]);
    let proof = Proof::from_zkapi_wire(&wire).unwrap();
    assert_eq!(
        run(
            &near_vk(KeyKind::Request),
            &near_inputs(&first.inputs),
            &proof
        ),
        Ok(false)
    );
    assert!(!native_accepts(KeyKind::Request, &first.inputs, &wire));
}

#[test]
fn public_inputs_at_or_above_the_modulus_are_refused_before_the_pairing() {
    let case = &cases()[0];
    let vk = near_vk(case.kind);
    let proof = Proof::from_zkapi_wire(&case.proof).unwrap();
    // r itself is the same element as zero modulo r, but must still be refused.
    let mut le_r = SCALAR_MODULUS_BE;
    le_r.reverse();
    let mut inputs = near_inputs(&case.inputs);
    inputs[0] = le_r;
    assert_eq!(
        run(&vk, &inputs, &proof),
        Err(VerifyError::NonCanonicalInput)
    );
    inputs[0] = [0xff; 32];
    assert_eq!(
        run(&vk, &inputs, &proof),
        Err(VerifyError::NonCanonicalInput)
    );
    // The wire parser refuses it too.
    assert_eq!(
        public_input_from_wire(&SCALAR_MODULUS_BE),
        Err(CodecError::NonCanonical)
    );
}

#[test]
fn wrong_public_input_counts_are_refused() {
    let case = &cases()[0];
    let vk = near_vk(case.kind);
    let proof = Proof::from_zkapi_wire(&case.proof).unwrap();
    let inputs = near_inputs(&case.inputs);
    assert_eq!(
        run(&vk, &inputs[..11], &proof),
        Err(VerifyError::InputCount)
    );
    let mut extra = inputs.clone();
    extra.push([0; 32]);
    assert_eq!(run(&vk, &extra, &proof), Err(VerifyError::InputCount));
    assert_eq!(run(&vk, &[], &proof), Err(VerifyError::InputCount));
}

#[test]
fn the_point_at_infinity_is_refused() {
    let case = &cases()[0];
    let vk = near_vk(case.kind);
    let inputs = near_inputs(&case.inputs);
    let good = Proof::from_zkapi_wire(&case.proof).unwrap();
    for broken in [
        Proof {
            a: [0; 64],
            ..good.clone()
        },
        Proof {
            b: [0; 128],
            ..good.clone()
        },
        Proof {
            c: [0; 64],
            ..good.clone()
        },
    ] {
        assert_eq!(run(&vk, &inputs, &broken), Err(VerifyError::Infinity));
    }
    // And in the key, a hand-built one (from_canonical refuses it on parse).
    let mut broken_vk = vk.clone();
    broken_vk.ic[3] = [0; 64];
    assert_eq!(run(&broken_vk, &inputs, &good), Err(VerifyError::Infinity));
    let mut broken_vk = vk.clone();
    broken_vk.gamma_g2 = [0; 128];
    assert_eq!(run(&broken_vk, &inputs, &good), Err(VerifyError::Infinity));
}

/// Malformed point encodings never verify: the verifier or the host refuses them,
/// and native arkworks agrees.
#[test]
fn malformed_points_never_verify() {
    let case = &cases()[0];
    let vk = near_vk(case.kind);
    let inputs = near_inputs(&case.inputs);
    let good_wire = case.proof.clone();
    let mut malformed: Vec<(String, Vec<u8>)> = Vec::new();
    // Off the curve: change a coordinate of A, B and C.
    for (name, word) in [("a.y", 1usize), ("b.x.c0", 2), ("b.y.c1", 5), ("c.x", 6)] {
        let mut wire = good_wire.clone();
        wire[32 * word + 31] ^= 1;
        malformed.push((format!("off curve {name}"), wire));
    }
    // Coordinates at or above p are refused by the codec before anything runs.
    for word in 0..8 {
        let mut wire = good_wire.clone();
        wire[32 * word..32 * (word + 1)].copy_from_slice(&BASE_MODULUS_BE);
        assert_eq!(Proof::from_zkapi_wire(&wire), Err(CodecError::NonCanonical));
        assert!(!native_accepts(case.kind, &case.inputs, &wire));
    }
    for (name, wire) in malformed {
        let proof = Proof::from_zkapi_wire(&wire).unwrap();
        assert!(!host_accepts(&vk, &inputs, &proof), "{name}");
        assert!(!native_accepts(case.kind, &case.inputs, &wire), "{name}");
    }
    // Wrong lengths.
    for len in [0usize, 255, 257] {
        let mut wire = good_wire.clone();
        wire.resize(len, 0);
        assert_eq!(Proof::from_zkapi_wire(&wire), Err(CodecError::Length));
    }
}

/// A point on the twist outside the order-r subgroup: the host enforces the G2
/// subgroup check (a host abort), so the verifier needs no multiplication by r.
#[test]
fn a_g2_point_outside_the_subgroup_never_verifies() {
    let case = &cases()[0];
    let vk = near_vk(case.kind);
    let inputs = near_inputs(&case.inputs);
    let mut found = None;
    for i in 1u64..200 {
        let x = Fq2::new(Fq::from(i), Fq::from(1u64));
        if let Some(p) = G2Affine::get_point_from_x_unchecked(x, false) {
            if p.is_on_curve() && !p.is_in_correct_subgroup_assuming_on_curve() {
                found = Some(p);
                break;
            }
        }
    }
    let bad = found.expect("a twist point outside the subgroup");
    let le = |f: &Fq| {
        let mut v = ark_ff::BigInteger::to_bytes_le(&f.into_bigint());
        v.resize(32, 0);
        v
    };
    let b: [u8; 128] = [le(&bad.x.c0), le(&bad.x.c1), le(&bad.y.c0), le(&bad.y.c1)]
        .concat()
        .try_into()
        .unwrap();
    let proof = Proof {
        b,
        ..Proof::from_zkapi_wire(&case.proof).unwrap()
    };
    assert!(!host_accepts(&vk, &inputs, &proof));
}

/// Verification never accepts what native arkworks rejects, over many mutations of
/// every fixture (single-bit flips in the proof and in each input word).
#[test]
fn differential_against_native_arkworks() {
    for case in cases() {
        let vk = near_vk(case.kind);
        let mut checked = 0;
        for byte in (0..256).step_by(7) {
            let mut wire = case.proof.clone();
            wire[byte] ^= 1 << (byte % 8);
            let ours = match Proof::from_zkapi_wire(&wire) {
                Ok(proof) => host_accepts(&vk, &near_inputs(&case.inputs), &proof),
                Err(_) => false,
            };
            assert_eq!(
                ours,
                native_accepts(case.kind, &case.inputs, &wire),
                "{} byte {byte}",
                case.name
            );
            checked += 1;
        }
        for index in 0..case.inputs.len() {
            let mut words = case.inputs.clone();
            words[index][31] ^= 1;
            let ours = match words
                .iter()
                .map(|w| public_input_from_wire(w))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(inputs) => {
                    host_accepts(&vk, &inputs, &Proof::from_zkapi_wire(&case.proof).unwrap())
                }
                Err(_) => false,
            };
            assert_eq!(
                ours,
                native_accepts(case.kind, &words, &case.proof),
                "{} input {index}",
                case.name
            );
            checked += 1;
        }
        assert!(checked > 30);
    }
}

/// Real proofs whose public inputs carry the codec's NEAR mappings verify
/// through the host functions, and the inputs hold exactly the mapped values.
#[test]
fn near_mapped_fixture_verifies() {
    use nymira_near_codec::{chain_id, contract_address, destination, Network};
    let fixture: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/near-fixture.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let chain = chain_id(Network::Testnet);
    let address = contract_address("vault.nymira.testnet");
    let payout = destination("alice.testnet");
    let mut count = 0;
    for (name, kind) in [
        ("request", KeyKind::Request),
        ("request_signed", KeyKind::Request),
        ("withdrawal", KeyKind::Withdrawal),
        ("escape_c0", KeyKind::Withdrawal),
        ("close", KeyKind::Withdrawal),
    ] {
        let case = &fixture[name];
        let inputs = &case["public_inputs"];
        assert_eq!(inputs["chain_id"].as_u64(), Some(chain), "{name}");
        assert_eq!(
            inputs["contract_address"]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
            hex::encode(address),
            "{name}"
        );
        if kind == KeyKind::Withdrawal && name != "close" {
            let dest: Vec<u8> = inputs["destination"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect();
            assert_eq!(dest, payout, "{name}");
        }
        let words: Vec<[u8; 32]> = hex_bytes(&case["inputs_abi"])
            .chunks(32)
            .map(|w| w.try_into().unwrap())
            .collect();
        // chain_id is input 1 and contract_address input 2 in both statements.
        assert_eq!(&words[1][24..], &chain.to_be_bytes(), "{name}");
        assert_eq!(words[2], address, "{name}");
        let proof = Proof::from_zkapi_wire(&hex_bytes(&case["proof_hex"])).unwrap();
        assert_eq!(
            run(&near_vk(kind), &near_inputs(&words), &proof),
            Ok(true),
            "{name}"
        );
        count += 1;
    }
    assert_eq!(count, 5);
}
