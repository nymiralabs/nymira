//! The provider adapter: the one internal seam, because NEAR AI cannot run in CI.
//!
//! Everything is keyed by lease ID. The key itself stays inside the adapter, and so does its
//! lifetime ([`KEY_LIFETIME_SECS`]); the operator only ever sees a key id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rand::RngCore;

use crate::clock::Clock;

/// How long a minted key lives at the provider. Longer than the 10-minute lease cap, so a key
/// never dies under a live lease and never outlives a forgotten one for long.
pub const KEY_LIFETIME_SECS: u64 = 15 * 60;

/// The key cap in nano-USD for a lease bound in micro-USD: `bound / 1.03`, floored, with integer
/// arithmetic (the 3% margin is a fixed constant).
pub fn key_cap_nano_usd(bound_micro_usd: u128) -> u128 {
    bound_micro_usd * 1_000 * 100 / 103
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("provider rejected the call: {0}")]
    Rejected(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
}

/// What the operator records about a minted key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintedKey {
    /// Random name unrelated to the lease id and the lease token.
    pub key_id: String,
    pub expires_at: u64,
}

/// Measured spend of one lease key, in nano-USD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyUsage {
    pub cost_nano_usd: u128,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Mints a key for `lease_id`, capped at [`key_cap_nano_usd`]`(bound)`.
    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError>;
    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError>;
    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError>;
    /// The lease ids of every live key, for the start-up sweep.
    async fn list(&self) -> Result<Vec<String>, ProviderError>;
    /// The organisation's credit in micro-USD.
    async fn balance(&self) -> Result<u128, ProviderError>;
}

/// A key as the fake recorded it.
#[derive(Clone, Debug)]
pub struct FakeKey {
    pub key_id: String,
    pub cap_nano_usd: u128,
    pub expires_at: u64,
    pub revoked: bool,
}

#[derive(Default)]
struct FakeState {
    keys: HashMap<String, FakeKey>,
    usage: HashMap<String, u128>,
    balance_micro_usd: u128,
    mint_delay: Duration,
    mint_error: Option<ProviderError>,
    balance_error: Option<ProviderError>,
    mint_calls: u32,
}

/// Records keys and returns configurable usage, balance, delays and errors.
pub struct FakeProvider {
    clock: Arc<dyn Clock>,
    state: Mutex<FakeState>,
}

impl FakeProvider {
    pub fn new(clock: Arc<dyn Clock>, balance_micro_usd: u128) -> Self {
        Self {
            clock,
            state: Mutex::new(FakeState {
                balance_micro_usd,
                ..FakeState::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_balance(&self, micro_usd: u128) {
        self.state().balance_micro_usd = micro_usd;
    }

    pub fn set_usage(&self, lease_id: &str, cost_nano_usd: u128) {
        self.state().usage.insert(lease_id.into(), cost_nano_usd);
    }

    pub fn set_mint_delay(&self, delay: Duration) {
        self.state().mint_delay = delay;
    }

    pub fn set_mint_error(&self, error: Option<ProviderError>) {
        self.state().mint_error = error;
    }

    pub fn set_balance_error(&self, error: Option<ProviderError>) {
        self.state().balance_error = error;
    }

    /// Every key ever minted, by lease id.
    pub fn keys(&self) -> HashMap<String, FakeKey> {
        self.state().keys.clone()
    }

    pub fn live_keys(&self) -> usize {
        self.state().keys.values().filter(|k| !k.revoked).count()
    }

    pub fn mint_calls(&self) -> u32 {
        self.state().mint_calls
    }

    /// A key that exists at the provider with no record at the operator: what a crash between
    /// minting and writing leaves behind.
    pub fn orphan_key(&self, lease_id: &str, cap_nano_usd: u128) {
        self.state().keys.insert(
            lease_id.into(),
            FakeKey {
                key_id: random_name(),
                cap_nano_usd,
                expires_at: self.clock.now_secs() + KEY_LIFETIME_SECS,
                revoked: false,
            },
        );
    }
}

fn random_name() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("k-{}", hex::encode(bytes))
}

#[async_trait]
impl ProviderAdapter for FakeProvider {
    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError> {
        let (delay, error) = {
            let mut state = self.state();
            state.mint_calls += 1;
            (state.mint_delay, state.mint_error.clone())
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if let Some(error) = error {
            return Err(error);
        }
        let key = FakeKey {
            key_id: random_name(),
            cap_nano_usd: key_cap_nano_usd(bound),
            expires_at: self.clock.now_secs() + KEY_LIFETIME_SECS,
            revoked: false,
        };
        let minted = MintedKey {
            key_id: key.key_id.clone(),
            expires_at: key.expires_at,
        };
        self.state().keys.insert(lease_id.into(), key);
        Ok(minted)
    }

    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError> {
        let state = self.state();
        if !state.keys.contains_key(lease_id) {
            return Err(ProviderError::Rejected("unknown lease".into()));
        }
        Ok(KeyUsage {
            cost_nano_usd: state.usage.get(lease_id).copied().unwrap_or(0),
        })
    }

    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError> {
        match self.state().keys.get_mut(lease_id) {
            Some(key) => key.revoked = true,
            None => return Err(ProviderError::Rejected("unknown lease".into())),
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, ProviderError> {
        let state = self.state();
        Ok(state
            .keys
            .iter()
            .filter(|(_, key)| !key.revoked)
            .map(|(lease, _)| lease.clone())
            .collect())
    }

    async fn balance(&self) -> Result<u128, ProviderError> {
        let state = self.state();
        match &state.balance_error {
            Some(error) => Err(error.clone()),
            None => Ok(state.balance_micro_usd),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::key_cap_nano_usd;

    #[test]
    fn the_key_cap_is_bound_over_1_03_in_nano_usd_rounded_down() {
        // 0.10 USDC = 100_000 micro-USD -> 97_087_378.64 nano-USD
        assert_eq!(key_cap_nano_usd(100_000), 97_087_378);
        assert_eq!(key_cap_nano_usd(500_000), 485_436_893);
        assert_eq!(key_cap_nano_usd(1), 970);
        assert_eq!(key_cap_nano_usd(0), 0);
    }
}
