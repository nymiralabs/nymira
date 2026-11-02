//! Fixed test keys and the E2EE test scheme.
//!
//! The wire layout and the suite follow NEAR AI's E2EE guide: X25519, HKDF-SHA256,
//! XChaCha20-Poly1305, `[eph_pk 32][nonce 24][ciphertext+tag]`, hex on the wire.
//! The guide does not give the HKDF salt or info string, so this fake uses no salt
//! and [`HKDF_INFO`]. A client that follows the layout but not that info string
//! will not decrypt here, and this fake does not claim to interoperate with the
//! real gateway.

use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305, XNonce};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

pub const HKDF_INFO: &[u8] = b"ed25519_encryption";

/// Signs `provider_tee` signatures and is listed in the model attestation.
pub fn model_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}
/// Signs `gateway` signatures and is listed in the gateway attestation.
pub fn gateway_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[2; 32])
}
/// Stands in for the Intel and NVIDIA evidence chain: signs every attestation entry.
pub fn attestation_root_key() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}
/// Signs when the `unbound_key` switch is on. Listed in no attestation.
pub fn rogue_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[4; 32])
}
/// The model's E2EE secret (the scalar of its signing key). Public so test clients can check
/// what the fake decrypted.
pub fn model_e2ee_secret() -> [u8; 32] {
    model_signing_key().to_scalar_bytes()
}

/// The model's E2EE public key (hex, Ed25519), published in the attestation report. It is the
/// model's signing address.
pub fn model_e2ee_public_hex() -> String {
    verifying_hex(&model_signing_key())
}

/// The X25519 key for an Ed25519 public key.
pub fn ed25519_to_x25519(pk: &[u8; 32]) -> Option<[u8; 32]> {
    Some(
        ed25519_dalek::VerifyingKey::from_bytes(pk)
            .ok()?
            .to_montgomery()
            .to_bytes(),
    )
}
pub fn verifying_hex(k: &SigningKey) -> String {
    hex::encode(k.verifying_key().as_bytes())
}

fn cipher(shared: &[u8; 32]) -> XChaCha20Poly1305 {
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(None, shared)
        .expand(HKDF_INFO, &mut key)
        .expect("32 bytes is a valid HKDF length");
    XChaCha20Poly1305::new((&key).into())
}

/// Encrypts to an Ed25519 public key with a fresh ephemeral key.
pub fn encrypt_to(recipient: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let eph = StaticSecret::random_from_rng(OsRng);
    let recipient = ed25519_to_x25519(recipient).expect("a valid Ed25519 public key");
    let shared = eph.diffie_hellman(&PublicKey::from(recipient));
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher(shared.as_bytes())
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .expect("encryption does not fail");
    [PublicKey::from(&eph).as_bytes().as_slice(), &nonce, &ct].concat()
}

/// Reverses [`encrypt_to`] for the holder of the X25519 secret.
pub fn decrypt(secret: &[u8; 32], wire: &[u8]) -> Result<Vec<u8>, &'static str> {
    if wire.len() < 32 + 24 + 16 {
        return Err("ciphertext too short");
    }
    let (eph_pk, rest) = wire.split_at(32);
    let (nonce, ct) = rest.split_at(24);
    let eph_pk: [u8; 32] = eph_pk.try_into().expect("split at 32");
    let shared = StaticSecret::from(*secret).diffie_hellman(&PublicKey::from(eph_pk));
    cipher(shared.as_bytes())
        .decrypt(XNonce::from_slice(nonce), ct)
        .map_err(|_| "authentication failed")
}

pub fn encrypt_hex(recipient: &[u8; 32], plaintext: &str) -> String {
    hex::encode(encrypt_to(recipient, plaintext.as_bytes()))
}

pub fn decrypt_hex(secret: &[u8; 32], text: &str) -> Result<String, &'static str> {
    let wire = hex::decode(text).map_err(|_| "not hex")?;
    String::from_utf8(decrypt(secret, &wire)?).map_err(|_| "not UTF-8")
}

pub fn parse_key32(hex_str: &str) -> Option<[u8; 32]> {
    hex::decode(hex_str).ok()?.try_into().ok()
}

/// A key pair for a test client: `(secret, Ed25519 public key)`.
pub fn client_keypair() -> ([u8; 32], [u8; 32]) {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let k = SigningKey::from_bytes(&seed);
    (k.to_scalar_bytes(), k.verifying_key().to_bytes())
}
