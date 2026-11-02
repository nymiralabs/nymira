//! Independent local-provider crypto for integrated wire tests (fixed test-only keys).
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm,
};
use chacha20poly1305::XChaCha20Poly1305;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use nearai_inference_sdk::SigningAlgo;
use sha2::{Digest, Sha256, Sha512};

fn derived(shared: &[u8], algo: SigningAlgo) -> [u8; 32] {
    let mut key = [0; 32];
    hkdf::Hkdf::<Sha256>::new(None, shared)
        .expand(
            match algo {
                SigningAlgo::Ed25519 => b"ed25519_encryption",
                SigningAlgo::Ecdsa => b"ecdsa_encryption",
            },
            &mut key,
        )
        .unwrap();
    key
}
pub fn decrypt_request(cipher: &str, algo: SigningAlgo) -> String {
    let cipher = hex::decode(cipher).unwrap();
    let plain = match algo {
        SigningAlgo::Ed25519 => {
            let scalar: [u8; 32] = Sha512::digest([7; 32])[..32].try_into().unwrap();
            let secret = x25519_dalek::StaticSecret::from(scalar);
            let public: [u8; 32] = cipher[..32].try_into().unwrap();
            let shared = secret.diffie_hellman(&public.into());
            XChaCha20Poly1305::new((&derived(shared.as_bytes(), algo)).into())
                .decrypt(cipher[32..56].into(), &cipher[56..])
                .unwrap()
        }
        SigningAlgo::Ecdsa => {
            let secret = k256::SecretKey::from_slice(&[7; 32]).unwrap();
            let public = k256::PublicKey::from_sec1_bytes(&cipher[..65]).unwrap();
            let shared = k256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            Aes256Gcm::new((&derived(shared.raw_secret_bytes(), algo)).into())
                .decrypt(cipher[65..77].into(), &cipher[77..])
                .unwrap()
        }
    };
    String::from_utf8(plain).unwrap()
}
pub fn encrypt_response(public: &str, algo: SigningAlgo) -> String {
    let public = hex::decode(public).unwrap();
    let mut out = match algo {
        SigningAlgo::Ed25519 => {
            let public = curve25519_dalek::edwards::CompressedEdwardsY(public.try_into().unwrap())
                .decompress()
                .unwrap()
                .to_montgomery();
            let secret = x25519_dalek::StaticSecret::from([3; 32]);
            let shared = secret.diffie_hellman(&public.to_bytes().into());
            let nonce = [2; 24];
            let cipher = XChaCha20Poly1305::new((&derived(shared.as_bytes(), algo)).into())
                .encrypt((&nonce).into(), b"Hello".as_slice())
                .unwrap();
            let mut out = x25519_dalek::PublicKey::from(&secret).as_bytes().to_vec();
            out.extend(nonce);
            out.extend(cipher);
            out
        }
        SigningAlgo::Ecdsa => {
            let mut bytes = vec![4];
            bytes.extend(public);
            let public = k256::PublicKey::from_sec1_bytes(&bytes).unwrap();
            let secret = k256::SecretKey::from_slice(&[3; 32]).unwrap();
            let shared = k256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            let nonce = [2; 12];
            let cipher = Aes256Gcm::new((&derived(shared.raw_secret_bytes(), algo)).into())
                .encrypt((&nonce).into(), b"Hello".as_slice())
                .unwrap();
            let mut out = secret
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            out.extend(nonce);
            out.extend(cipher);
            out
        }
    };
    hex::encode(std::mem::take(&mut out))
}
pub fn ecdsa_identity(seed: u8) -> (String, String) {
    let secret = k256::SecretKey::from_slice(&[seed; 32]).unwrap();
    let public = secret.public_key().to_encoded_point(false);
    (
        hex::encode(&sha3::Keccak256::digest(&public.as_bytes()[1..])[12..]),
        hex::encode(&public.as_bytes()[1..]),
    )
}
pub fn ecdsa_signature(text: &str, seed: u8) -> String {
    let secret = k256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
    let mut message = format!("\x19Ethereum Signed Message:\n{}", text.len()).into_bytes();
    message.extend(text.as_bytes());
    let (sig, id) = secret
        .sign_prehash_recoverable(&sha3::Keccak256::digest(message))
        .unwrap();
    let mut bytes = sig.to_bytes().to_vec();
    bytes.push(u8::from(id) + 27);
    hex::encode(bytes)
}
