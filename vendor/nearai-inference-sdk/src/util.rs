use rand::RngCore;
use sha2::{Digest, Sha256, Sha384};

pub const NONCE_BYTES: usize = 32;

pub fn generate_nonce() -> String {
    let mut bytes = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn decode_hex(value: &str) -> Result<Vec<u8>, ()> {
    let normalized = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    hex::decode(normalized).map_err(|_| ())
}

pub fn require_hex_length(value: &str, length: usize) -> Result<Vec<u8>, ()> {
    let bytes = decode_hex(value)?;
    if bytes.len() != length {
        return Err(());
    }
    Ok(bytes)
}

pub fn sha256(value: impl AsRef<[u8]>) -> Vec<u8> {
    Sha256::digest(value.as_ref()).to_vec()
}

pub fn sha384(value: impl AsRef<[u8]>) -> Vec<u8> {
    Sha384::digest(value.as_ref()).to_vec()
}
