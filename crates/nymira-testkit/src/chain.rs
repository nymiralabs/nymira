//! Sandboxes at the nearcore release of mainnet or testnet.
//!
//! `near_workspaces::sandbox()` runs near-sandbox 0.3.16's default release, which is the
//! mainnet one. Use [`sandbox_for`] when a test must run at a stated chain.

use near_workspaces::network::Sandbox;
use near_workspaces::Worker;

/// nearcore release at mainnet's protocol version (86 on 2026-10-25).
pub const MAINNET_NEARCORE: &str = "2.13.4";
/// nearcore release at testnet's protocol version (88 on 2026-10-25).
pub const TESTNET_NEARCORE: &str = "2.14.0-rc.5";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    Mainnet,
    Testnet,
}

impl Chain {
    pub fn nearcore_version(self) -> &'static str {
        match self {
            Chain::Mainnet => MAINNET_NEARCORE,
            Chain::Testnet => TESTNET_NEARCORE,
        }
    }
}

/// A sandbox running the nearcore release of `chain`.
pub async fn sandbox_for(chain: Chain) -> Result<Worker<Sandbox>, Box<dyn std::error::Error>> {
    Ok(near_workspaces::sandbox_with_version(chain.nearcore_version()).await?)
}
