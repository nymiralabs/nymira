//! Mainnet-parity fixture: the wasm of the real mainnet USDC contract (Circle
//! FiatToken, account `17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1`),
//! fetched once with `view_code` on 2026-11-01 and checked against its code hash. The
//! testnet USDC has a different hash ([`TESTNET_CODE_HASH`]), so a test that passes on
//! testnet alone does not show mainnet USDC behaviour.

use near_workspaces::network::Sandbox;
use near_workspaces::{Contract, Worker};
use sha2::{Digest, Sha256};

use crate::build::workspace_root;

pub const MAINNET_ACCOUNT: &str =
    "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";
/// Code hash of [`MAINNET_ACCOUNT`] (base58), read from mainnet RPC.
pub const MAINNET_CODE_HASH: &str = "DVJx7F3dHEHJYwpYPwXHu8K9Uo7h4kXFcALucY5oNX8q";
/// Code hash of Circle's testnet USDC (`3e2210e1...b8af`), for contrast.
pub const TESTNET_CODE_HASH: &str = "EoAUFWELjD9gYzsjKDXYRmHDHf1RJ95jaKNSYE6v9b2z";
/// SHA-256 of the fixture wasm.
pub const WASM_SHA256: &str = "b98ccd530f7e643cfef32f4529ade060ef87ff73b0101536399470777b86cfe6";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The fixture wasm, checked against [`WASM_SHA256`].
pub fn wasm() -> Vec<u8> {
    let wasm =
        std::fs::read(workspace_root().join("crates/nymira-testkit/fixtures/mainnet-usdc.wasm"))
            .expect("mainnet USDC fixture is readable");
    let digest: String = Sha256::digest(&wasm)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(digest, WASM_SHA256, "the mainnet USDC fixture changed");
    wasm
}

/// Deploys the real contract on a fresh sandbox account and initializes it with the
/// sandbox root in every role, 6 decimals and symbol USDC.
pub async fn deploy(worker: &Worker<Sandbox>) -> Result<Contract> {
    let contract = worker.dev_deploy(&wasm()).await?;
    let root = worker.root_account()?.id().to_string();
    contract
        .call("init")
        .args_json(serde_json::json!({
            "admin_ids": [root],
            "master_minter_ids": [root],
            "owner_ids": [root],
            "pauser_ids": [root],
            "blocklister_id": root,
            "metadata": {
                "spec": "ft-1.0.0",
                "name": "USDC",
                "symbol": "USDC",
                "decimals": 6,
                "icon": null,
                "reference": null,
                "reference_hash": null,
            },
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(contract)
}
