//! `0x` plus 64 lowercase hex digits: how the vault spells 32-byte words in JSON.

pub fn encode32(word: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(66);
    out.push_str("0x");
    for byte in word {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 15)] as char);
    }
    out
}

/// Exactly `0x` and 64 hex digits, either case.
pub fn decode32(text: &str) -> Option<[u8; 32]> {
    let digits = text.strip_prefix("0x")?.as_bytes();
    if digits.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(digits.chunks(2)) {
        *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}
