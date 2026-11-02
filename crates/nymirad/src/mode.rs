//! `NYMIRA_MODE` picks the network and nothing else. There is no mock value: mock builds are a
//! compile-time feature, and a release binary has none.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Testnet,
    Mainnet,
}

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

    /// The only token account the deposit accepts on this network, if the mode fixes it.
    pub fn pinned_token(self) -> Option<&'static str> {
        match self {
            Self::Testnet => Some(TESTNET_USDC),
            Self::Mainnet => None,
        }
    }
}
