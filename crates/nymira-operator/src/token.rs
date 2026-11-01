//! Lease tokens: 128 random bits, stored hashed, compared in constant time.

use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A fresh token (32 lowercase hex digits) and its SHA-256.
pub fn generate() -> (String, Vec<u8>) {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let hash = hash(&token);
    (token, hash)
}

pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Whether `presented` hashes to `stored`, without an early exit.
pub fn matches(stored: &[u8], presented: &str) -> bool {
    stored.ct_eq(&hash(presented)).into()
}

pub fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_matches_its_own_hash_only() {
        let (token, stored) = generate();
        assert_eq!(token.len(), 32);
        assert!(matches(&stored, &token));
        assert!(!matches(&stored, &generate().0));
        assert!(!matches(&stored, ""));
        assert_ne!(
            token,
            hex::encode(&stored),
            "the stored value is not the token"
        );
    }
}
