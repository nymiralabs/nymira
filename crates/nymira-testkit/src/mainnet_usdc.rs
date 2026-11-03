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
    let mainnet = mainnet_worker().await?;
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

/// Like [`deploy`], but also sets up a minter so tests can credit balances. The contract's
/// roles need two approvals, so two accounts share every role; two controllers approve the
/// minter's allowance. Returns the contract and the minter account.
pub async fn deploy_minting(
    worker: &Worker<Sandbox>,
) -> Result<(
    Contract,
    near_workspaces::Account,
    near_workspaces::Account,
    u64,
)> {
    use serde_json::json;

    let contract = import_with_retry(worker).await?;
    let root = worker.root_account()?;
    let second = worker.dev_create_account().await?;
    let controllers = [
        worker.dev_create_account().await?,
        worker.dev_create_account().await?,
    ];
    let both = vec![root.id().to_string(), second.id().to_string()];
    contract
        .call("init")
        .args_json(json!({
            "admin_ids": both,
            "master_minter_ids": both,
            "owner_ids": both,
            "pauser_ids": both,
            "blocklister_id": root.id(),
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
    async fn step(
        who: &near_workspaces::Account,
        contract: &Contract,
        method: &str,
        args: serde_json::Value,
    ) -> Result<()> {
        who.call(contract.id(), method)
            .args_json(args)
            .max_gas()
            .transact()
            .await?
            .into_result()?;
        Ok(())
    }
    // Each controller is configured for the root minter by the two owners; the minter's
    // allowance is then set by the two controllers.
    for (request_id, controller) in controllers.iter().enumerate() {
        step(
            &root,
            &contract,
            "create_multisig_request",
            json!({"action": {"ConfigureController": {
            "controller_id": controller.id(), "minter_id": root.id()}}}),
        )
        .await?;
        step(
            &root,
            &contract,
            "approve_multisig_request",
            json!({"request_id": request_id}),
        )
        .await?;
        step(
            &second,
            &contract,
            "approve_multisig_request",
            json!({"request_id": request_id}),
        )
        .await?;
        step(
            &root,
            &contract,
            "execute_multisig_request",
            json!({"request_id": request_id}),
        )
        .await?;
    }
    let request_id = controllers.len();
    step(
        &controllers[0],
        &contract,
        "create_multisig_request",
        json!({"action": {"ConfigureMinterAllowance": {
        "controller_id": controllers[0].id(), "minter_allowance": "1000000000000000"}}}),
    )
    .await?;
    for controller in &controllers {
        step(
            controller,
            &contract,
            "approve_multisig_request",
            json!({"request_id": request_id}),
        )
        .await?;
    }
    step(
        &controllers[0],
        &contract,
        "execute_multisig_request",
        json!({"request_id": request_id}),
    )
    .await?;
    // The three requests above used ids 0 to 2; the next free id is returned.
    Ok((contract, root.clone(), second, request_id as u64 + 1))
}

/// The public mainnet RPC rate-limits parallel test runs, so the import backs off and retries.
async fn import_with_retry(worker: &Worker<Sandbox>) -> Result<Contract> {
    let id: AccountId = MAINNET_ACCOUNT.parse()?;
    let mut last = None;
    for attempt in 1..=8u64 {
        let mainnet = mainnet_worker().await?;
        match worker.import_contract(&id, &mainnet).transact().await {
            Ok(contract) => return Ok(contract),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3 * attempt)).await;
    }
    Err(last.expect("at least one attempt").into())
}

/// A read-only mainnet worker. `rpc.mainnet.near.org` rate-limits almost at once, so the default
/// is FastNEAR's public node; `NYMIRA_MAINNET_RPC` overrides it.
async fn mainnet_worker() -> Result<Worker<near_workspaces::network::Mainnet>> {
    let url = std::env::var("NYMIRA_MAINNET_RPC")
        .unwrap_or_else(|_| "https://rpc.mainnet.fastnear.com".to_owned());
    Ok(near_workspaces::mainnet().rpc_addr(&url).await?)
}
