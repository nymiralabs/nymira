//! Mainnet-parity fixture: the real mainnet USDC contract (Circle FiatToken, account
//! `17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1`), copied into the
//! sandbox from mainnet RPC at the same account id and checked against its code hash. The
//! testnet USDC has a different hash ([`TESTNET_CODE_HASH`]), so a test that passes on
//! testnet alone does not show mainnet USDC behaviour. The wasm is not stored in this
//! repository: it is third-party bytecode, fetched on use. Needs network access to mainnet RPC.

use near_workspaces::network::Sandbox;
use near_workspaces::{AccountId, Contract, Worker};

pub const MAINNET_ACCOUNT: &str =
    "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";
/// Code hash of [`MAINNET_ACCOUNT`] (base58), read from mainnet RPC on 2026-11-01.
pub const MAINNET_CODE_HASH: &str = "DVJx7F3dHEHJYwpYPwXHu8K9Uo7h4kXFcALucY5oNX8q";
/// Code hash of Circle's testnet USDC (`3e2210e1...b8af`), for contrast.
pub const TESTNET_CODE_HASH: &str = "EoAUFWELjD9gYzsjKDXYRmHDHf1RJ95jaKNSYE6v9b2z";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Copies the real contract's code from mainnet into the sandbox at the same account id,
/// checks its code hash, and initializes it with the sandbox root in every role, 6 decimals
/// and symbol USDC.
pub async fn deploy(worker: &Worker<Sandbox>) -> Result<Contract> {
    let mainnet = near_workspaces::mainnet().await?;
    let id: AccountId = MAINNET_ACCOUNT.parse()?;
    let contract = worker.import_contract(&id, &mainnet).transact().await?;
    let hash = match contract.view_account().await?.contract_state {
        near_workspaces::ContractState::LocalHash(hash) => hash.to_string(),
        other => return Err(format!("unexpected contract state {other:?}").into()),
    };
    if hash != MAINNET_CODE_HASH {
        return Err(
            format!("mainnet USDC code hash is {hash}, expected {MAINNET_CODE_HASH}").into(),
        );
    }
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
