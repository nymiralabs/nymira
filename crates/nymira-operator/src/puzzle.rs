//! Hashcash bound to the status slot: no identity, works over Tor.
//!
//! The header `X-Nymira-Puzzle` is `<slot nonce>:<counter>`. It is valid when the nonce is one of
//! the recent slots' nonces and `SHA-256("<nonce>:<counter>")` starts with `difficulty` zero bits.

use sha2::{Digest, Sha256};

pub fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

pub fn digest(nonce: &str, counter: &str) -> [u8; 32] {
    Sha256::digest(format!("{nonce}:{counter}").as_bytes()).into()
}

/// Whether `value` solves the puzzle at `difficulty` for one of `valid_nonces`.
pub fn check(value: &str, difficulty: u32, valid_nonces: &[String]) -> bool {
    let Some((nonce, counter)) = value.split_once(':') else {
        return false;
    };
    counter.len() <= 32
        && valid_nonces.iter().any(|n| n == nonce)
        && leading_zero_bits(&digest(nonce, counter)) >= difficulty
}

/// Finds a counter for `nonce` (clients; tests).
pub fn solve(nonce: &str, difficulty: u32) -> String {
    (0u64..)
        .map(|n| n.to_string())
        .find(|counter| leading_zero_bits(&digest(nonce, counter)) >= difficulty)
        .map(|counter| format!("{nonce}:{counter}"))
        .expect("a counter exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solution_binds_to_its_slot_nonce() {
        let valid = vec!["aa".to_string(), "bb".to_string()];
        let solved = solve("aa", 8);
        assert!(check(&solved, 8, &valid));
        assert!(check(&solved, 0, &valid));
        assert!(
            !check(&solved, 8, &["bb".to_string()]),
            "another slot's nonce"
        );
        assert!(!check("no-colon", 0, &valid));
        assert_eq!(leading_zero_bits(&[0, 0b0010_0000]), 10);
    }
}
