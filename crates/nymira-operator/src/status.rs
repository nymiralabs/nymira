//! The status nonce: one value per 5-second slot, signed under the state key with its own
//! domain tag so it cannot parse as a state or clearance signature.
//!
//! The same cached value is served to everyone in a slot, so no per-client value exists that the
//! operator could later match to a lease or clearance request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use zkapi_core::v2::{domain_value, hash_felts};
use zkapi_proof::compact::CompactSigner;
use zkapi_types::{Felt252, SchnorrSignature};

pub const SLOT_SECS: u64 = 5;
/// Domain tag of the nonce signature.
pub const STATUS_DOMAIN: &[u8] = b"nymira/status/v1";
/// Slots (including the current one) whose nonce still solves a puzzle: 30 seconds.
const PUZZLE_SLOTS: u64 = 6;

#[derive(Clone, Debug)]
pub struct SlotValue {
    pub nonce: String,
    pub host: String,
    pub ts: u64,
    pub sig: SchnorrSignature,
}

fn small_felt(bytes: &[u8]) -> Felt252 {
    // At most 31 bytes: always below the field modulus.
    let mut word = [0u8; 32];
    word[32 - bytes.len()..].copy_from_slice(bytes);
    Felt252(word)
}

/// The signed message: `Poseidon(domain, nonce, H(host), ts)`, four inputs where a state
/// message has seven and a clearance message five.
pub fn status_message(nonce: &str, host: &str, ts: u64) -> Felt252 {
    let nonce_bytes = hex::decode(nonce).unwrap_or_default();
    let host_hash = Sha256::digest(host.as_bytes());
    hash_felts(&[
        domain_value(STATUS_DOMAIN),
        small_felt(&nonce_bytes[..nonce_bytes.len().min(16)]),
        small_felt(&host_hash[..31]),
        Felt252::from_u64(ts),
    ])
}

/// What signs a status nonce under the state key: the signer process in production. It builds
/// the message itself from the three fields, so it is no oracle for other messages.
#[async_trait::async_trait]
pub trait StateSigning: Send + Sync {
    fn public_key(&self) -> zkapi_types::wire::CurvePointWire;
    async fn sign_status(&self, nonce: &str, host: &str, ts: u64) -> Option<SchnorrSignature>;
}

#[async_trait::async_trait]
impl StateSigning for CompactSigner {
    fn public_key(&self) -> zkapi_types::wire::CurvePointWire {
        CompactSigner::public_key(self)
    }

    async fn sign_status(&self, nonce: &str, host: &str, ts: u64) -> Option<SchnorrSignature> {
        Some(self.sign(&status_message(nonce, host, ts)))
    }
}

pub struct StatusSigner {
    signer: Arc<dyn StateSigning>,
    host: String,
    secret: [u8; 32],
    cache: Mutex<HashMap<u64, SlotValue>>,
}

impl StatusSigner {
    pub fn new(signer: Arc<dyn StateSigning>, host: impl Into<String>) -> Self {
        let mut secret = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut secret);
        Self {
            signer,
            host: host.into(),
            secret,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn nonce_for(&self, slot: u64) -> String {
        let mut h = Sha256::new();
        h.update(self.secret);
        h.update(slot.to_be_bytes());
        hex::encode(&h.finalize()[..16])
    }

    /// The signed value of the slot containing `now_secs`; `None` when the signer cannot be
    /// reached and the slot has no value yet.
    pub async fn current(&self, now_secs: u64) -> Option<SlotValue> {
        let slot = now_secs / SLOT_SECS;
        {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|s, _| *s + 2 >= slot);
            if let Some(value) = cache.get(&slot) {
                return Some(value.clone());
            }
        }
        let nonce = self.nonce_for(slot);
        let ts = slot * SLOT_SECS;
        let sig = self.signer.sign_status(&nonce, &self.host, ts).await?;
        let value = SlotValue {
            nonce,
            host: self.host.clone(),
            ts,
            sig,
        };
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(slot, value.clone());
        Some(value)
    }

    /// Nonces a puzzle may be bound to: the last 30 seconds of slots.
    pub fn recent_nonces(&self, now_secs: u64) -> Vec<String> {
        let slot = now_secs / SLOT_SECS;
        (slot.saturating_sub(PUZZLE_SLOTS - 1)..=slot)
            .map(|s| self.nonce_for(s))
            .collect()
    }
}
