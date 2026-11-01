//! Cheap point checks and the Groth16 seam.

use std::path::Path;

use ark_ed_on_bn254::EdwardsAffine;
use ark_ff::PrimeField;
use zkapi_proof::compact::RequestVerifier;
use zkapi_types::wire::Groth16ProofWire;
use zkapi_types::{Felt252, RequestPublicInputsV2};

/// The pairing check, behind a trait so tests can hold verifications in flight.
pub trait ProofVerifier: Send + Sync {
    fn verify(&self, public: &RequestPublicInputsV2, proof: &Groth16ProofWire) -> bool;
}

pub struct GrothVerifier(RequestVerifier);

impl GrothVerifier {
    /// Loads `request.vk` from a setup directory. Development keys load only in a `dev-setup`
    /// build.
    pub fn load(dir: &Path) -> Result<Self, String> {
        RequestVerifier::load(dir)
            .map(Self)
            .map_err(|e| e.to_string())
    }
}

impl ProofVerifier for GrothVerifier {
    fn verify(&self, public: &RequestPublicInputsV2, proof: &Groth16ProofWire) -> bool {
        self.0.verify(public, proof).unwrap_or(false)
    }
}

fn point_ok(x: &Felt252, y: &Felt252) -> bool {
    let field = |f: &Felt252| ark_bn254::Fr::from_be_bytes_mod_order(f.as_bytes());
    let point = EdwardsAffine::new_unchecked(field(x), field(y));
    point.is_on_curve() && point.is_in_correct_subgroup_assuming_on_curve()
}

/// The statement's two curve points (the state key and the anonymous commitment) are on the
/// curve and in the subgroup. Felt fields are canonical by deserialization.
pub fn point_checks(public: &RequestPublicInputsV2) -> bool {
    point_ok(&public.state_signing_key_x, &public.state_signing_key_y)
        && point_ok(
            &public.anonymous_commitment_x,
            &public.anonymous_commitment_y,
        )
}
