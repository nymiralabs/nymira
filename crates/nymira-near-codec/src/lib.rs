//! Maps zkapi's EVM-shaped public inputs to NEAR values and converts proofs
//! and verifying keys to the byte format of NEAR's `alt_bn128` host functions.

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use sha2::{Digest, Sha256};

/// The chain a vault is deployed on. It fixes the circuit's `chain_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

fn sha256(input: &str) -> [u8; 32] {
    Sha256::digest(input.as_bytes()).into()
}

/// First 8 bytes of `sha256("near:testnet")` (`"near:mainnet"` on mainnet),
/// read big-endian.
pub fn chain_id(network: Network) -> u64 {
    let label = match network {
        Network::Testnet => "near:testnet",
        Network::Mainnet => "near:mainnet",
    };
    u64::from_be_bytes(sha256(label)[..8].try_into().unwrap())
}

/// `sha256(vault account id)` reduced modulo the BN254 scalar field, as a
/// 32-byte big-endian field element (zkapi's `Felt252` layout).
pub fn contract_address(vault_account_id: &str) -> [u8; 32] {
    let reduced = Fr::from_be_bytes_mod_order(&sha256(vault_account_id));
    let mut out = [0u8; 32];
    let raw = reduced.into_bigint().to_bytes_be();
    out[32 - raw.len()..].copy_from_slice(&raw);
    out
}

/// First 20 bytes of `sha256(payout account id)`: the circuit's 160-bit
/// `destination`.
pub fn destination(payout_account_id: &str) -> [u8; 20] {
    sha256(payout_account_id)[..20].try_into().unwrap()
}

/// Every circuit-facing time is Unix seconds: NEAR's `block_timestamp`
/// (nanoseconds) divided by 10^9. The codec has no nanosecond field.
pub fn unix_seconds(block_timestamp_ns: u64) -> u64 {
    block_timestamp_ns / 1_000_000_000
}

/// `note_id * 2^192 + deposit * 2^64 + expiry` as a 32-byte big-endian word:
/// the leaf-bound withdrawal's `noteFields` input.
pub fn note_fields(note_id: u32, deposit_amount: u128, expiry: u64) -> [u8; 32] {
    zkapi_types::pack_note_fields(note_id, deposit_amount, expiry).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn chain_id_is_the_first_eight_sha256_bytes_big_endian() {
        assert_eq!(chain_id(Network::Testnet), 7_718_972_759_422_244_814);
        assert_eq!(chain_id(Network::Mainnet), 11_955_663_840_136_396_698);
    }

    #[test]
    fn contract_address_is_sha256_reduced_to_the_field() {
        assert_eq!(
            hex(&contract_address("vault.nymira.testnet")),
            "2a65065f1eaad4681d3c1d96524e57fc77e645542824b93f3406f437dc746207"
        );
        // This digest is above the modulus, so the reduction changes it.
        assert_eq!(
            hex(&contract_address("vault.nymira.near")),
            "102f2fd23933c459339426e689976b38a531f662c4a11b0db57e1fbbf0c757e6"
        );
    }

    #[test]
    fn destination_is_the_first_twenty_sha256_bytes() {
        assert_eq!(
            hex(&destination("alice.near")),
            "2dd5dda540767b3a1aa33544bcba38042f4df6de"
        );
        assert_eq!(
            hex(&destination("vault.nymira.near")),
            "a15c1b2adcc8a4d65c84f80a0e1b74501dcdaf3c"
        );
    }

    #[test]
    fn time_is_unix_seconds_from_block_nanoseconds() {
        assert_eq!(unix_seconds(1_700_000_000_999_999_999), 1_700_000_000);
        assert_eq!(unix_seconds(0), 0);
        assert_eq!(unix_seconds(u64::MAX), 18_446_744_073);
    }

    #[test]
    fn note_fields_packs_id_deposit_and_expiry() {
        assert_eq!(
            hex(&note_fields(0, 1_000_000, 4_000_000_000)),
            "0000000000000000000000000000000000000000000f424000000000ee6b2800"
        );
        assert_eq!(
            hex(&note_fields(7, 5_000_000, 1_900_000_000)),
            "0000000000000007000000000000000000000000004c4b4000000000713fb300"
        );
        assert_eq!(
            hex(&note_fields(u32::MAX, u128::MAX, u64::MAX)),
            "00000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        );
    }
}
