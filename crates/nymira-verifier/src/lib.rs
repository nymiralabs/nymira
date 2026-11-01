//! Groth16 verification over NEAR's `alt_bn128` host functions.
//!
//! The host refuses points that are off the curve, G2 points outside the
//! order-r subgroup and coordinates at or above the base modulus (the call
//! aborts). It accepts the point at infinity, so this crate refuses it. Public
//! inputs are checked here, before any host call.

use near_sdk::env;
use nymira_near_codec::{Proof, VerifyingKey, SCALAR_MODULUS_BE};

/// Why a proof was refused before the pairing check. A proof that reaches the
/// check and fails it is `Ok(false)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The number of public inputs is not `vk.ic.len() - 1`.
    InputCount,
    /// A public input is at or above the scalar modulus.
    NonCanonicalInput,
    /// A point of the proof or the key is the point at infinity.
    Infinity,
}

fn is_infinity(point: &[u8]) -> bool {
    point.iter().all(|byte| *byte == 0)
}

/// `a >= modulus` for 32-byte little-endian values.
fn at_or_above_scalar_modulus(le: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        let m = SCALAR_MODULUS_BE[31 - i];
        if le[i] != m {
            return le[i] > m;
        }
    }
    true
}

/// Checks `e(-A, B) * e(alpha, beta) * e(vk_x, gamma) * e(C, delta) == 1` with
/// `vk_x = ic[0] + sum(input_i * ic[i + 1])`. `public_inputs` are 32-byte
/// little-endian scalars (see `nymira_near_codec::public_input_from_wire`).
pub fn verify(
    vk: &VerifyingKey,
    public_inputs: &[[u8; 32]],
    proof: &Proof,
) -> Result<bool, VerifyError> {
    if public_inputs.len() + 1 != vk.ic.len() {
        return Err(VerifyError::InputCount);
    }
    if public_inputs.iter().any(at_or_above_scalar_modulus) {
        return Err(VerifyError::NonCanonicalInput);
    }
    if is_infinity(&proof.a)
        || is_infinity(&proof.b)
        || is_infinity(&proof.c)
        || is_infinity(&vk.alpha_g1)
        || is_infinity(&vk.beta_g2)
        || is_infinity(&vk.gamma_g2)
        || is_infinity(&vk.delta_g2)
        || vk.ic.iter().any(|point| is_infinity(point))
    {
        return Err(VerifyError::Infinity);
    }

    // vk_x in one multiexp: (ic[0], 1), (ic[i], input_i)...
    let mut multiexp = Vec::with_capacity(96 * vk.ic.len());
    let mut one = [0u8; 32];
    one[0] = 1;
    multiexp.extend_from_slice(&vk.ic[0]);
    multiexp.extend_from_slice(&one);
    for (point, input) in vk.ic[1..].iter().zip(public_inputs) {
        multiexp.extend_from_slice(point);
        multiexp.extend_from_slice(input);
    }
    let vk_x = env::alt_bn128_g1_multiexp(&multiexp);

    // -A through g1_sum with the sign byte set.
    let mut negated = Vec::with_capacity(65);
    negated.push(1u8);
    negated.extend_from_slice(&proof.a);
    let neg_a = env::alt_bn128_g1_sum(&negated);

    let mut pairs = Vec::with_capacity(4 * 192);
    for (g1, g2) in [
        (&neg_a[..], &proof.b[..]),
        (&vk.alpha_g1[..], &vk.beta_g2[..]),
        (&vk_x[..], &vk.gamma_g2[..]),
        (&proof.c[..], &vk.delta_g2[..]),
    ] {
        pairs.extend_from_slice(g1);
        pairs.extend_from_slice(g2);
    }
    Ok(env::alt_bn128_pairing_check(&pairs))
}
