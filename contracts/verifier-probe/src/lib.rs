//! Sandbox probe for `nymira-verifier`.

use near_sdk::json_types::Base64VecU8;
use near_sdk::{env, near, require};
use nymira_near_codec::{public_input_from_wire, Proof, VerifyingKey};

#[near(contract_state)]
#[derive(Default)]
pub struct Probe {}

#[near]
impl Probe {
    /// Verifies a zkapi wire proof. `inputs` are 32-byte big-endian words. Logs
    /// the gas of the key conversion and of the verification separately.
    pub fn verify(
        &self,
        vk: Base64VecU8,
        inputs: Vec<Base64VecU8>,
        proof: Base64VecU8,
    ) -> bool {
        let start = env::used_gas().as_gas();
        let vk = VerifyingKey::from_canonical(&vk.0).unwrap_or_else(|_| env::panic_str("bad vk"));
        let converted = env::used_gas().as_gas();
        let inputs: Vec<[u8; 32]> = inputs
            .iter()
            .map(|word| public_input_from_wire(&word.0).unwrap_or_else(|_| env::panic_str("bad input")))
            .collect();
        let proof = Proof::from_zkapi_wire(&proof.0).unwrap_or_else(|_| env::panic_str("bad proof"));
        let before = env::used_gas().as_gas();
        let result = nymira_verifier::verify(&vk, &inputs, &proof);
        let after = env::used_gas().as_gas();
        env::log_str(&format!(
            "gas vk_decode={} verify={}",
            converted - start,
            after - before
        ));
        require!(result.is_ok(), "refused");
        result.unwrap()
    }
}
