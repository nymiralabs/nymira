//! Usage records: what the minting process read from the provider, signed with its own key.
//!
//! The signer accepts a charge only against such a record, so the public API process cannot make
//! it sign a charge of its own choosing.

use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};

const DOMAIN: &[u8] = b"nymira/usage/v1\n";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub lease_id: String,
    /// Nano-USD the key spent; `None` when usage could not be read and the lease settles at its
    /// bound.
    pub cost_nano_usd: Option<String>,
    pub read_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedUsage {
    pub record: UsageRecord,
    pub sig: String,
}

fn message(record: &UsageRecord) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(record).expect("record serialises"));
    bytes
}

#[derive(Clone)]
pub struct Minter(SigningKey);

impl Minter {
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Self(SigningKey::from_bytes(&seed))
    }

    pub fn public_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }

    pub fn sign(&self, record: UsageRecord) -> SignedUsage {
        let sig = hex::encode(self.0.sign(&message(&record)).to_bytes());
        SignedUsage { record, sig }
    }
}

/// Whatever signs usage records: the minting process's key, reached over its socket in
/// production.
#[async_trait::async_trait]
pub trait UsageSigner: Send + Sync {
    async fn sign(&self, record: UsageRecord) -> Result<SignedUsage, String>;
}

#[async_trait::async_trait]
impl UsageSigner for Minter {
    async fn sign(&self, record: UsageRecord) -> Result<SignedUsage, String> {
        Ok(Minter::sign(self, record))
    }
}

pub fn verify(key: &VerifyingKey, usage: &SignedUsage) -> bool {
    let Ok(bytes) = hex::decode(&usage.sig) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(&bytes) else {
        return false;
    };
    key.verify(&message(&usage.record), &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_verifies_only_under_its_minter_and_unaltered() {
        let (minter, other) = (Minter::generate(), Minter::generate());
        let signed = minter.sign(UsageRecord {
            lease_id: "l".into(),
            cost_nano_usd: Some("5".into()),
            read_at: 1,
        });
        assert!(verify(&minter.public_key(), &signed));
        assert!(!verify(&other.public_key(), &signed));
        let mut forged = signed.clone();
        forged.record.cost_nano_usd = Some("4".into());
        assert!(!verify(&minter.public_key(), &forged));
        forged = signed;
        forged.sig = "00".into();
        assert!(!verify(&minter.public_key(), &forged));
    }
}
