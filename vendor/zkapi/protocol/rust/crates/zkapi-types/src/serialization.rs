//! Serialization helpers for wire formats.

use crate::Felt252;

/// Encode an Ethereum address (20 bytes) as a felt252.
pub fn address_to_felt(addr: &[u8; 20]) -> Felt252 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr);
    Felt252(bytes)
}

/// Extract an Ethereum address from a felt252.
/// Returns None if the upper 12 bytes are not zero.
pub fn felt_to_address(f: &Felt252) -> Option<[u8; 20]> {
    if f.0[..12] != [0u8; 12] {
        return None;
    }
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&f.0[12..32]);
    Some(addr)
}

/// Encode a boolean as a felt252 (0 or 1).
pub fn bool_to_felt(b: bool) -> Felt252 {
    if b {
        Felt252::ONE
    } else {
        Felt252::ZERO
    }
}

/// Decode a felt252 as a boolean.
pub fn felt_to_bool(f: &Felt252) -> Option<bool> {
    if *f == Felt252::ZERO {
        Some(false)
    } else if *f == Felt252::ONE {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_roundtrip() {
        let addr = [
            0xdeu8, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
        let felt = address_to_felt(&addr);
        let back = felt_to_address(&felt).unwrap();
        assert_eq!(addr, back);
    }

    #[test]
    fn test_bool_roundtrip() {
        assert_eq!(felt_to_bool(&bool_to_felt(true)), Some(true));
        assert_eq!(felt_to_bool(&bool_to_felt(false)), Some(false));
        assert_eq!(felt_to_bool(&Felt252::from_u64(2)), None);
    }
}
