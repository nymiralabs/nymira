//! `nymira-near-codec` for the browser. The plain Rust functions below are the
//! API (and are unit-tested natively); the `wasm` module wraps each one for
//! JavaScript with wasm-bindgen.
//!
//! Byte formats are the codec's (see `crates/nymira-near-codec/VECTORS.md`).
//! Composite results are flat byte strings:
//!
//! - proof: `a (64) || b (128) || c (64)`, 256 bytes, host little-endian words;
//! - verifying key: `alpha_g1 (64) || beta_g2 (128) || gamma_g2 (128) ||
//!   delta_g2 (128) || ic[0] (64) || ic[1] (64) ...`, at least 512 bytes, the
//!   `ic` count being the remainder divided by 64.

use nymira_near_codec::{CodecError, Network, Proof, VerifyingKey};

/// `"testnet"` or `"mainnet"`.
fn network(name: &str) -> Result<Network, String> {
    match name {
        "testnet" => Ok(Network::Testnet),
        "mainnet" => Ok(Network::Mainnet),
        other => Err(format!(
            "unknown network {other:?}, expected testnet or mainnet"
        )),
    }
}

fn refuse(error: CodecError) -> String {
    match error {
        CodecError::Length => "wrong byte length",
        CodecError::NonCanonical => "field element at or above its modulus",
        CodecError::Infinity => "point at infinity",
        CodecError::Malformed => "malformed encoding",
    }
    .to_string()
}

pub fn chain_id(network_name: &str) -> Result<u64, String> {
    Ok(nymira_near_codec::chain_id(network(network_name)?))
}

pub fn contract_address(vault_account_id: &str) -> Vec<u8> {
    nymira_near_codec::contract_address(vault_account_id).to_vec()
}

pub fn destination(payout_account_id: &str) -> Vec<u8> {
    nymira_near_codec::destination(payout_account_id).to_vec()
}

pub fn unix_seconds(block_timestamp_ns: u64) -> u64 {
    nymira_near_codec::unix_seconds(block_timestamp_ns)
}

pub fn note_fields(note_id: u32, deposit_amount: u128, expiry: u64) -> Vec<u8> {
    nymira_near_codec::note_fields(note_id, deposit_amount, expiry).to_vec()
}

/// zkapi wire proof (256 bytes, big-endian words) to the host proof bytes.
pub fn proof_from_wire(wire: &[u8]) -> Result<Vec<u8>, String> {
    let proof = Proof::from_zkapi_wire(wire).map_err(refuse)?;
    Ok([&proof.a[..], &proof.b[..], &proof.c[..]].concat())
}

/// Canonical (arkworks compressed) verifying key bytes to the host key bytes.
pub fn verifying_key_from_canonical(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let vk = VerifyingKey::from_canonical(bytes).map_err(refuse)?;
    let mut out = [
        &vk.alpha_g1[..],
        &vk.beta_g2[..],
        &vk.gamma_g2[..],
        &vk.delta_g2[..],
    ]
    .concat();
    for point in &vk.ic {
        out.extend_from_slice(point);
    }
    Ok(out)
}

/// One big-endian public input word to the little-endian scalar.
pub fn public_input_from_wire(word: &[u8]) -> Result<Vec<u8>, String> {
    nymira_near_codec::public_input_from_wire(word)
        .map(|scalar| scalar.to_vec())
        .map_err(refuse)
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(js_name = chainId)]
    pub fn chain_id(network: &str) -> Result<u64, JsError> {
        super::chain_id(network).map_err(|e| JsError::new(&e))
    }

    #[wasm_bindgen(js_name = contractAddress)]
    pub fn contract_address(vault_account_id: &str) -> Vec<u8> {
        super::contract_address(vault_account_id)
    }

    #[wasm_bindgen]
    pub fn destination(payout_account_id: &str) -> Vec<u8> {
        super::destination(payout_account_id)
    }

    #[wasm_bindgen(js_name = unixSeconds)]
    pub fn unix_seconds(block_timestamp_ns: u64) -> u64 {
        super::unix_seconds(block_timestamp_ns)
    }

    #[wasm_bindgen(js_name = noteFields)]
    pub fn note_fields(note_id: u32, deposit_amount: u128, expiry: u64) -> Vec<u8> {
        super::note_fields(note_id, deposit_amount, expiry)
    }

    #[wasm_bindgen(js_name = proofFromWire)]
    pub fn proof_from_wire(wire: &[u8]) -> Result<Vec<u8>, JsError> {
        super::proof_from_wire(wire).map_err(|e| JsError::new(&e))
    }

    #[wasm_bindgen(js_name = verifyingKeyFromCanonical)]
    pub fn verifying_key_from_canonical(bytes: &[u8]) -> Result<Vec<u8>, JsError> {
        super::verifying_key_from_canonical(bytes).map_err(|e| JsError::new(&e))
    }

    #[wasm_bindgen(js_name = publicInputFromWire)]
    pub fn public_input_from_wire(word: &[u8]) -> Result<Vec<u8>, JsError> {
        super::public_input_from_wire(word).map_err(|e| JsError::new(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ec::AffineRepr;
    use ark_serialize::CanonicalSerialize;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn network_names() {
        assert_eq!(chain_id("testnet"), Ok(7_718_972_759_422_244_814));
        assert_eq!(chain_id("mainnet"), Ok(11_955_663_840_136_396_698));
        assert!(chain_id("localnet").is_err());
    }

    #[test]
    fn mappings_match_the_codec_vectors() {
        assert_eq!(
            hex(&contract_address("vault.nymira.near")),
            "102f2fd23933c459339426e689976b38a531f662c4a11b0db57e1fbbf0c757e6"
        );
        assert_eq!(
            hex(&destination("alice.near")),
            "2dd5dda540767b3a1aa33544bcba38042f4df6de"
        );
        assert_eq!(unix_seconds(1_700_000_000_999_999_999), 1_700_000_000);
        assert_eq!(
            hex(&note_fields(7, 5_000_000, 1_900_000_000)),
            "0000000000000007000000000000000000000000004c4b4000000000713fb300"
        );
    }

    #[test]
    fn proof_and_input_conversions() {
        // A wire proof of words equal to 1 converts to little-endian words.
        let mut wire = [0u8; 256];
        for word in wire.chunks_mut(32) {
            word[31] = 1;
        }
        let host = proof_from_wire(&wire).unwrap();
        assert_eq!(host.len(), 256);
        assert!(host
            .chunks(32)
            .all(|w| w[0] == 1 && w[1..].iter().all(|b| *b == 0)));
        assert!(proof_from_wire(&wire[..255]).is_err());
        assert!(public_input_from_wire(&[0xff; 32]).is_err());
        assert_eq!(public_input_from_wire(&wire[..32]).unwrap()[0], 1);
    }

    /// The key the JavaScript self-test converts: (canonical bytes, host bytes).
    fn generator_vk() -> (Vec<u8>, Vec<u8>) {
        let g1 = ark_bn254::G1Affine::generator();
        let g2 = ark_bn254::G2Affine::generator();
        let vk = ark_groth16::VerifyingKey::<ark_bn254::Bn254> {
            alpha_g1: g1,
            beta_g2: g2,
            gamma_g2: g2,
            delta_g2: g2,
            gamma_abc_g1: vec![g1, g1],
        };
        let mut canonical = Vec::new();
        vk.serialize_compressed(&mut canonical).unwrap();
        let host = verifying_key_from_canonical(&canonical).unwrap();
        (canonical, host)
    }

    fn vk_fixture_json() -> String {
        let (canonical, host) = generator_vk();
        format!(
            "{{\"canonical\":\"{}\",\"host\":\"{}\"}}",
            hex(&canonical),
            hex(&host)
        )
    }

    #[test]
    fn verifying_key_layout() {
        let (canonical, host) = generator_vk();
        assert_eq!(host.len(), 64 + 3 * 128 + 2 * 64);
        assert_eq!(host[0], 1);
        assert_eq!(host[32], 2);
        assert!(verifying_key_from_canonical(&canonical[1..]).is_err());
    }

    /// Regenerate the file with
    /// `cargo test -p nymira-near-codec-wasm print_vk_fixture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_vk_fixture() {
        println!("{}", vk_fixture_json());
    }

    #[test]
    fn js_fixture_is_current() {
        let file = include_str!("../../../web-testkit/test/fixtures/generator-vk.json");
        assert_eq!(file.trim(), vk_fixture_json());
    }
}
