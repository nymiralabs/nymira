//! The open-source Intents Verifier v0.4.2 (`near/intents`, tag `defuse/v0.4.2`, MIT) as a
//! sandbox fixture, so tickets 20 and 22 can test the `intents.near` branch end to end:
//! deposit into the Verifier, `ft_withdraw(..., msg)`, and the Verifier's `ft_transfer_call`
//! of USDC into the vault.
//!
//! Always call it the open-source Verifier v0.4.2, never "the mainnet contract": its code
//! hash differs from the deployed `intents.near`. The vault's test-only Intents account
//! setting (feature `test-intents-account`) names the account this fixture runs at.

use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Contract, Worker};
use sha2::{Digest, Sha256};

use crate::build::workspace_root;

/// SHA-256 of the release wasm (`defuse.wasm` of `defuse/v0.4.2`), as published.
pub const VERIFIER_SHA256: &str =
    "f67f9559b3b512c52dfa11723a28b9119bcf773ac01780370d6409e5f1fb20e5";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The fixture wasm, checked against [`VERIFIER_SHA256`].
pub fn verifier_wasm() -> Vec<u8> {
    let wasm =
        std::fs::read(workspace_root().join("crates/nymira-testkit/fixtures/verifier-v0.4.2.wasm"))
            .expect("verifier fixture is readable");
    let digest: String = Sha256::digest(&wasm)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, VERIFIER_SHA256, "the Verifier fixture changed");
    wasm
}

/// Deploys and initializes the Verifier at a subaccount of the sandbox root named
/// `account_name` (for example `intents`), with `wnear_id` as its wNEAR and no fee.
pub async fn deploy_verifier(
    worker: &Worker<Sandbox>,
    account_name: &str,
    wnear_id: &str,
) -> Result<Contract> {
    let root = worker.root_account()?;
    let account = root
        .create_subaccount(account_name)
        .initial_balance(NearToken::from_near(50))
        .transact()
        .await?
        .into_result()?;
    let contract = account.deploy(&verifier_wasm()).await?.into_result()?;
    contract
        .call("new")
        .args_json(serde_json::json!({
            "config": {
                "wnear_id": wnear_id,
                "fees": { "fee": 0, "fee_collector": root.id() },
                "roles": {
                    "super_admins": [root.id()],
                    "admins": {},
                    "grantees": {}
                }
            }
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(contract)
}
