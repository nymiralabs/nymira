//! Vault stub: proves the pinned toolchain builds, deploys and runs on the
//! NEAR sandbox. The real vault replaces it.

use near_sdk::near;

#[cfg(all(feature = "mainnet", feature = "dev-setup"))]
compile_error!("the mainnet profile must not enable dev-setup");
#[cfg(all(feature = "judge", feature = "dev-setup"))]
compile_error!("the public judge vault and testnet bundle must not enable dev-setup");
#[cfg(all(feature = "mainnet", feature = "test-intents-account"))]
compile_error!("the mainnet profile must not enable test-intents-account");

#[near(contract_state)]
#[derive(Default)]
pub struct Vault {}

#[near]
impl Vault {
    /// Identifies the stub build.
    pub fn stub_version(&self) -> u32 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_version_is_one() {
        assert_eq!(Vault::default().stub_version(), 1);
    }
}
