//! `NYMIRA_MODE` picks the network and nothing else. There is no mock value: mock builds are a
//! compile-time feature, and a release binary has none.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Testnet,
    Mainnet,
}

/// Which Intents funding path a build offers. Both paths are always built and tested; this picks
/// the one the mainnet profile carries, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentsPath {
    /// 1Click pays the vault in one step.
    Direct,
    /// 1Click pays the user's own account, then `nymirad` sends the USDC to the vault.
    TwoStep,
    /// No Intents path: fund with NEAR USDC only.
    #[serde(rename = "none")]
    Off,
}

/// The mainnet profile's path, fixed at build time (issue 50). `TwoStep` because 1Click has given
/// no written answer on the vault as a recipient and the mainnet probe has not run, so this is
/// untested live.
pub const MAINNET_INTENTS_PATH: IntentsPath = IntentsPath::TwoStep;

/// Circle's testnet USDC. `usdc.fakes.testnet` is never used.
pub const TESTNET_USDC: &str = "3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af";
pub const FAUCET: &str = "faucet.circle.com";

#[derive(Debug, thiserror::Error)]
#[error("NYMIRA_MODE must be `testnet` or `mainnet`, not `{0}`")]
pub struct BadMode(pub String);

impl Mode {
    pub fn parse(text: &str) -> Result<Self, BadMode> {
        match text {
            "testnet" => Ok(Self::Testnet),
            "mainnet" => Ok(Self::Mainnet),
            other => Err(BadMode(other.into())),
        }
    }

    /// Reads `NYMIRA_MODE`; testnet when unset (the hackathon deployment).
    pub fn from_env() -> Result<Self, BadMode> {
        match std::env::var("NYMIRA_MODE") {
            Ok(text) => Self::parse(&text),
            Err(_) => Ok(Self::Testnet),
        }
    }

    /// Goes into the derivation of every note secret, so the two networks never share one.
    pub fn chain_label(self) -> &'static str {
        match self {
            Self::Testnet => "near-testnet",
            Self::Mainnet => "near-mainnet",
        }
    }

    /// The Intents funding paths run in the mainnet profile only.
    pub fn intents_enabled(self) -> bool {
        self == Self::Mainnet
    }

    /// The path this build offers on this network. Testnet is always quote-only.
    pub fn intents_path(self) -> IntentsPath {
        match self {
            Self::Testnet => IntentsPath::Off,
            Self::Mainnet => MAINNET_INTENTS_PATH,
        }
    }

    /// The only token account the deposit accepts on this network, if the mode fixes it.
    pub fn pinned_token(self) -> Option<&'static str> {
        match self {
            Self::Testnet => Some(TESTNET_USDC),
            Self::Mainnet => None,
        }
    }
}
