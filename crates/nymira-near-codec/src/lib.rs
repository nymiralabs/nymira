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

/// BN254 base field modulus `p`, big-endian.
pub const BASE_MODULUS_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58, 0x5d,
    0x97, 0x81, 0x6a, 0x91, 0x68, 0x71, 0xca, 0x8d, 0x3c, 0x20, 0x8c, 0x16, 0xd8, 0x7c, 0xfd, 0x47,
];

/// BN254 scalar field modulus `r`, big-endian.
pub const SCALAR_MODULUS_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58, 0x5d,
    0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93, 0xf0, 0x00, 0x00, 0x01,
];

/// Why a byte string was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// Wrong byte length.
    Length,
    /// A field element at or above its modulus.
    NonCanonical,
    /// The point at infinity where a finite point is required.
    Infinity,
    /// Not a valid arkworks encoding (bad point, trailing bytes, ...).
    Malformed,
}

/// Affine G1 point for the host functions: `x || y`, each 32 bytes little-endian.
pub type G1 = [u8; 64];
/// Affine G2 point for the host functions: `x.c0 || x.c1 || y.c0 || y.c1`, each
/// 32 bytes little-endian.
pub type G2 = [u8; 128];

fn le_word<F: PrimeField>(value: &F) -> [u8; 32] {
    let mut out = [0u8; 32];
    let raw = value.into_bigint().to_bytes_le();
    out[..raw.len()].copy_from_slice(&raw);
    out
}

fn g1_bytes(point: &ark_bn254::G1Affine) -> Result<G1, CodecError> {
    if point.infinity {
        return Err(CodecError::Infinity);
    }
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&le_word(&point.x));
    out[32..].copy_from_slice(&le_word(&point.y));
    Ok(out)
}

fn g2_bytes(point: &ark_bn254::G2Affine) -> Result<G2, CodecError> {
    if point.infinity {
        return Err(CodecError::Infinity);
    }
    let mut out = [0u8; 128];
    out[..32].copy_from_slice(&le_word(&point.x.c0));
    out[32..64].copy_from_slice(&le_word(&point.x.c1));
    out[64..96].copy_from_slice(&le_word(&point.y.c0));
    out[96..].copy_from_slice(&le_word(&point.y.c1));
    Ok(out)
}

/// A Groth16 proof in the host byte format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    pub a: G1,
    pub b: G2,
    pub c: G1,
}

impl Proof {
    /// From an arkworks proof. Fails on the point at infinity.
    pub fn from_arkworks(proof: &ark_groth16::Proof<ark_bn254::Bn254>) -> Result<Self, CodecError> {
        Ok(Self {
            a: g1_bytes(&proof.a)?,
            b: g2_bytes(&proof.b)?,
            c: g1_bytes(&proof.c)?,
        })
    }

    /// From zkapi's wire proof: 8 big-endian 32-byte words (`a.x a.y b.x.c0
    /// b.x.c1 b.y.c0 b.y.c1 c.x c.y`). Every word must be below the base
    /// modulus. Curve and subgroup membership are checked by the host.
    pub fn from_zkapi_wire(wire: &[u8]) -> Result<Self, CodecError> {
        if wire.len() != 256 {
            return Err(CodecError::Length);
        }
        let mut le = [0u8; 256];
        for (word, out) in wire.chunks(32).zip(le.chunks_mut(32)) {
            if word >= &BASE_MODULUS_BE[..] {
                return Err(CodecError::NonCanonical);
            }
            for (dst, src) in out.iter_mut().zip(word.iter().rev()) {
                *dst = *src;
            }
        }
        Ok(Self {
            a: le[..64].try_into().unwrap(),
            b: le[64..192].try_into().unwrap(),
            c: le[192..].try_into().unwrap(),
        })
    }
}

/// A Groth16 verifying key in the host byte format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyingKey {
    pub alpha_g1: G1,
    pub beta_g2: G2,
    pub gamma_g2: G2,
    pub delta_g2: G2,
    pub ic: Vec<G1>,
}

impl VerifyingKey {
    /// From an arkworks key. Fails on the point at infinity or an empty `ic`.
    pub fn from_arkworks(
        vk: &ark_groth16::VerifyingKey<ark_bn254::Bn254>,
    ) -> Result<Self, CodecError> {
        if vk.gamma_abc_g1.is_empty() {
            return Err(CodecError::Malformed);
        }
        Ok(Self {
            alpha_g1: g1_bytes(&vk.alpha_g1)?,
            beta_g2: g2_bytes(&vk.beta_g2)?,
            gamma_g2: g2_bytes(&vk.gamma_g2)?,
            delta_g2: g2_bytes(&vk.delta_g2)?,
            ic: vk
                .gamma_abc_g1
                .iter()
                .map(g1_bytes)
                .collect::<Result<_, _>>()?,
        })
    }

    /// From the canonical verifying key bytes: arkworks compressed
    /// serialization of the key, the string zkapi's pin check hashes
    /// (`CEREMONY_VK_KECCAK256`). arkworks checks curve and subgroup
    /// membership; trailing bytes are refused. This is the only way the
    /// vault builds a key.
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, CodecError> {
        use ark_serialize::CanonicalDeserialize;
        let mut reader = bytes;
        let vk = ark_groth16::VerifyingKey::<ark_bn254::Bn254>::deserialize_compressed(&mut reader)
            .map_err(|_| CodecError::Malformed)?;
        if !reader.is_empty() {
            return Err(CodecError::Malformed);
        }
        Self::from_arkworks(&vk)
    }
}

/// Checks an operator signing key (a Baby Jubjub point, `x` and `y` as
/// big-endian field words): both coordinates below the field modulus, the point
/// on the curve and in the prime-order subgroup, and not the identity.
pub fn check_signing_key(x: &[u8; 32], y: &[u8; 32]) -> Result<(), CodecError> {
    use ark_ed_on_bn254::{EdwardsAffine, Fq};
    if *x >= SCALAR_MODULUS_BE || *y >= SCALAR_MODULUS_BE {
        return Err(CodecError::NonCanonical);
    }
    let point = EdwardsAffine::new_unchecked(
        Fq::from_be_bytes_mod_order(x),
        Fq::from_be_bytes_mod_order(y),
    );
    if point.is_zero() {
        return Err(CodecError::Infinity);
    }
    if !point.is_on_curve() || !point.is_in_correct_subgroup_assuming_on_curve() {
        return Err(CodecError::Malformed);
    }
    Ok(())
}

/// A public input from its big-endian wire word to the little-endian scalar
/// the host's multiexp takes. Must be below the scalar modulus.
pub fn public_input_from_wire(word: &[u8]) -> Result<[u8; 32], CodecError> {
    if word.len() != 32 {
        return Err(CodecError::Length);
    }
    if word >= &SCALAR_MODULUS_BE[..] {
        return Err(CodecError::NonCanonical);
    }
    let mut out = [0u8; 32];
    for (dst, src) in out.iter_mut().zip(word.iter().rev()) {
        *dst = *src;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ec::AffineRepr;

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

    // Byte-format tests. BN254 G1 generator is (1, 2); G2 generator coordinates below.
    fn g1_generator_le() -> [u8; 64] {
        let mut out = [0u8; 64];
        out[0] = 1;
        out[32] = 2;
        out
    }

    fn g2_generator_le() -> [u8; 128] {
        let words = [
            "1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed",
            "198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2",
            "12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa",
            "090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b",
        ];
        let mut out = [0u8; 128];
        for (i, word) in words.iter().enumerate() {
            let mut bytes: Vec<u8> = (0..32)
                .map(|j| u8::from_str_radix(&word[2 * j..2 * j + 2], 16).unwrap())
                .collect();
            bytes.reverse();
            out[32 * i..32 * (i + 1)].copy_from_slice(&bytes);
        }
        out
    }

    /// zkapi wire proof words are big-endian: a.x a.y b.x.c0 b.x.c1 b.y.c0 b.y.c1 c.x c.y.
    fn wire_of(a: &[u8; 64], b: &[u8; 128], c: &[u8; 64]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in [&a[..], &b[..], &c[..]] {
            for word in part.chunks(32) {
                out.extend(word.iter().rev());
            }
        }
        out
    }

    #[test]
    fn arkworks_points_become_little_endian_host_bytes() {
        use ark_ec::AffineRepr;
        let g1 = ark_bn254::G1Affine::generator();
        let g2 = ark_bn254::G2Affine::generator();
        let proof = ark_groth16::Proof::<ark_bn254::Bn254> {
            a: g1,
            b: g2,
            c: g1,
        };
        let converted = Proof::from_arkworks(&proof).unwrap();
        assert_eq!(converted.a, g1_generator_le());
        assert_eq!(converted.b, g2_generator_le());
        assert_eq!(converted.c, g1_generator_le());
    }

    #[test]
    fn zkapi_wire_proof_converts_to_host_bytes() {
        let wire = wire_of(&g1_generator_le(), &g2_generator_le(), &g1_generator_le());
        assert_eq!(wire.len(), 256);
        let proof = Proof::from_zkapi_wire(&wire).unwrap();
        assert_eq!(proof.a, g1_generator_le());
        assert_eq!(proof.b, g2_generator_le());
        assert_eq!(proof.c, g1_generator_le());
    }

    #[test]
    fn wire_proof_with_wrong_length_is_rejected() {
        let wire = wire_of(&g1_generator_le(), &g2_generator_le(), &g1_generator_le());
        for len in [0, 255, 257, 512] {
            let mut bytes = wire.clone();
            bytes.resize(len, 0);
            assert_eq!(
                Proof::from_zkapi_wire(&bytes),
                Err(CodecError::Length),
                "{len}"
            );
        }
    }

    #[test]
    fn wire_proof_coordinate_at_or_above_the_base_modulus_is_rejected() {
        let p_be = BASE_MODULUS_BE;
        let wire = wire_of(&g1_generator_le(), &g2_generator_le(), &g1_generator_le());
        for word in 0..8 {
            for value in [p_be.to_vec(), vec![0xff; 32]] {
                let mut bytes = wire.clone();
                bytes[32 * word..32 * (word + 1)].copy_from_slice(&value);
                assert_eq!(
                    Proof::from_zkapi_wire(&bytes),
                    Err(CodecError::NonCanonical),
                    "word {word}"
                );
            }
        }
        // p - 1 is still a canonical encoding.
        let mut p_minus_one = p_be;
        p_minus_one[31] -= 1;
        let mut bytes = wire;
        bytes[..32].copy_from_slice(&p_minus_one);
        assert!(Proof::from_zkapi_wire(&bytes).is_ok());
    }

    #[test]
    fn public_input_must_be_below_the_scalar_modulus() {
        let mut one = [0u8; 32];
        one[31] = 1;
        let mut expected = [0u8; 32];
        expected[0] = 1;
        assert_eq!(public_input_from_wire(&one), Ok(expected));
        let mut r_minus_one = SCALAR_MODULUS_BE;
        r_minus_one[31] -= 1;
        assert!(public_input_from_wire(&r_minus_one).is_ok());
        assert_eq!(
            public_input_from_wire(&SCALAR_MODULUS_BE),
            Err(CodecError::NonCanonical)
        );
        assert_eq!(
            public_input_from_wire(&[0xff; 32]),
            Err(CodecError::NonCanonical)
        );
        assert_eq!(public_input_from_wire(&[0u8; 31]), Err(CodecError::Length));
    }

    #[test]
    fn verifying_key_rejects_trailing_and_truncated_bytes() {
        use ark_serialize::CanonicalSerialize;
        let g1 = ark_bn254::G1Affine::generator();
        let g2 = ark_bn254::G2Affine::generator();
        let vk = ark_groth16::VerifyingKey::<ark_bn254::Bn254> {
            alpha_g1: g1,
            beta_g2: g2,
            gamma_g2: g2,
            delta_g2: g2,
            gamma_abc_g1: vec![g1, g1],
        };
        let mut bytes = Vec::new();
        vk.serialize_compressed(&mut bytes).unwrap();
        let converted = VerifyingKey::from_canonical(&bytes).unwrap();
        assert_eq!(converted.alpha_g1, g1_generator_le());
        assert_eq!(converted.beta_g2, g2_generator_le());
        assert_eq!(converted.ic, vec![g1_generator_le(), g1_generator_le()]);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(VerifyingKey::from_canonical(&trailing).is_err());
        assert!(VerifyingKey::from_canonical(&bytes[..bytes.len() - 1]).is_err());
        assert!(VerifyingKey::from_canonical(&[]).is_err());
    }

    #[test]
    fn verifying_key_rejects_the_point_at_infinity() {
        use ark_serialize::CanonicalSerialize;
        let g1 = ark_bn254::G1Affine::generator();
        let g2 = ark_bn254::G2Affine::generator();
        let zero1 = ark_bn254::G1Affine::zero();
        let zero2 = ark_bn254::G2Affine::zero();
        let make = |alpha, beta, ic0| ark_groth16::VerifyingKey::<ark_bn254::Bn254> {
            alpha_g1: alpha,
            beta_g2: beta,
            gamma_g2: g2,
            delta_g2: g2,
            gamma_abc_g1: vec![ic0, g1],
        };
        for vk in [
            make(zero1, g2, g1),
            make(g1, zero2, g1),
            make(g1, g2, zero1),
        ] {
            let mut bytes = Vec::new();
            vk.serialize_compressed(&mut bytes).unwrap();
            assert_eq!(
                VerifyingKey::from_canonical(&bytes),
                Err(CodecError::Infinity)
            );
        }
    }

    #[test]
    fn signing_key_accepts_a_curve_point_and_refuses_the_rest() {
        use ark_ec::{AffineRepr, CurveGroup};
        use ark_ed_on_bn254::EdwardsAffine;
        let word = |value: &ark_ed_on_bn254::Fq| {
            let mut out = [0u8; 32];
            let raw = value.into_bigint().to_bytes_be();
            out[32 - raw.len()..].copy_from_slice(&raw);
            out
        };
        let key = (EdwardsAffine::generator() * ark_ed_on_bn254::Fr::from(42u64)).into_affine();
        let (x, y) = (word(&key.x), word(&key.y));
        assert_eq!(check_signing_key(&x, &y), Ok(()));
        // Off the curve.
        let mut bent = y;
        bent[31] ^= 1;
        assert_eq!(check_signing_key(&x, &bent), Err(CodecError::Malformed));
        // The identity (0, 1).
        let mut one = [0u8; 32];
        one[31] = 1;
        assert_eq!(check_signing_key(&[0u8; 32], &one), Err(CodecError::Infinity));
        // The order-2 point (0, -1) is on the curve but outside the prime-order subgroup.
        let minus_one = word(&-ark_ed_on_bn254::Fq::from(1u64));
        assert_eq!(
            check_signing_key(&[0u8; 32], &minus_one),
            Err(CodecError::Malformed)
        );
        // A coordinate at the modulus.
        assert_eq!(
            check_signing_key(&SCALAR_MODULUS_BE, &y),
            Err(CodecError::NonCanonical)
        );
    }
}
