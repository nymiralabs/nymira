//! The mock USDC token (`contracts/mock-usdc`) and a receiver for
//! `ft_transfer_call`, deployed on a near-workspaces sandbox.

use std::sync::OnceLock;

use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};

use crate::build::contract_wasm;

/// 0.00125 NEAR: the `storage_balance_bounds` minimum.
pub const MIN_STORAGE_DEPOSIT: u128 = 1_250_000_000_000_000_000_000;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn usdc_wasm() -> &'static [u8] {
    static WASM: OnceLock<Vec<u8>> = OnceLock::new();
    WASM.get_or_init(|| contract_wasm("contracts/mock-usdc"))
}

fn receiver_wasm() -> &'static [u8] {
    static WASM: OnceLock<Vec<u8>> = OnceLock::new();
    WASM.get_or_init(|| contract_wasm("contracts/mock-ft-receiver"))
}

/// A deployed mock token.
pub struct MockUsdc {
    pub contract: Contract,
    /// Set when the contract is the real mainnet USDC wasm: the account that mints.
    minter: Option<Account>,
}

impl MockUsdc {
    pub async fn deploy(worker: &Worker<Sandbox>) -> Result<Self> {
        Ok(Self {
            contract: worker.dev_deploy(usdc_wasm()).await?,
            minter: None,
        })
    }

    /// The real mainnet USDC wasm behind the same helpers (needs mainnet RPC); the pause,
    /// blocklist, panic and junk-return switches exist only on the mock.
    pub async fn deploy_real(worker: &Worker<Sandbox>) -> Result<Self> {
        let (contract, minter) = crate::mainnet_usdc::deploy_minting(worker).await?;
        Ok(Self {
            contract,
            minter: Some(minter),
        })
    }

    pub fn is_real(&self) -> bool {
        self.minter.is_some()
    }

    pub async fn storage_deposit(
        &self,
        caller: &Account,
        account_id: Option<&AccountId>,
        registration_only: bool,
        deposit: NearToken,
    ) -> Result<()> {
        caller
            .call(self.contract.id(), "storage_deposit")
            .args_json(serde_json::json!({
                "account_id": account_id,
                "registration_only": registration_only,
            }))
            .deposit(deposit)
            .max_gas()
            .transact()
            .await?
            .into_result()?;
        Ok(())
    }

    /// Registers `account` with exactly the minimum deposit.
    pub async fn register(&self, account: &Account) {
        self.storage_deposit(
            account,
            None,
            true,
            NearToken::from_yoctonear(MIN_STORAGE_DEPOSIT),
        )
        .await
        .expect("registration succeeds");
    }

    /// Registers an account that is not a signer here (a contract), paid by the token account.
    pub async fn register_id(&self, account_id: &AccountId) {
        self.storage_deposit(
            self.contract.as_account(),
            Some(account_id),
            true,
            NearToken::from_yoctonear(MIN_STORAGE_DEPOSIT),
        )
        .await
        .expect("registration succeeds");
    }

    pub async fn is_registered(&self, account_id: &AccountId) -> bool {
        let value: serde_json::Value = self
            .contract
            .view("storage_balance_of")
            .args_json(serde_json::json!({ "account_id": account_id }))
            .await
            .expect("view succeeds")
            .json()
            .expect("json");
        !value.is_null()
    }

    pub async fn storage_balance_bounds_min(&self) -> u128 {
        let value: serde_json::Value = self
            .contract
            .view("storage_balance_bounds")
            .await
            .expect("view succeeds")
            .json()
            .expect("json");
        value["min"]
            .as_str()
            .expect("min is a string")
            .parse()
            .expect("number")
    }

    /// Credits tokens without registration or checks.
    pub async fn mint(&self, account_id: &AccountId, amount: u128) {
        if let Some(minter) = &self.minter {
            minter
                .call(self.contract.id(), "mint")
                .args_json(serde_json::json!({ "to": account_id, "amount": amount.to_string() }))
                .max_gas()
                .transact()
                .await
                .expect("transaction runs")
                .into_result()
                .expect("mint succeeds");
            return;
        }
        self.contract
            .call("mint")
            .args_json(
                serde_json::json!({ "account_id": account_id, "amount": amount.to_string() }),
            )
            .transact()
            .await
            .expect("transaction runs")
            .into_result()
            .expect("mint succeeds");
    }

    pub async fn balance_of(&self, account_id: &AccountId) -> u128 {
        let value: String = self
            .contract
            .view("ft_balance_of")
            .args_json(serde_json::json!({ "account_id": account_id }))
            .await
            .expect("view succeeds")
            .json()
            .expect("json");
        value.parse().expect("number")
    }

    /// `ft_transfer` with the required 1 yocto. Returns the raw return bytes.
    pub async fn ft_transfer(
        &self,
        from: &Account,
        to: &AccountId,
        amount: u128,
    ) -> Result<Vec<u8>> {
        self.ft_transfer_with_deposit(from, to, amount, 1).await
    }

    pub async fn ft_transfer_with_deposit(
        &self,
        from: &Account,
        to: &AccountId,
        amount: u128,
        yocto: u128,
    ) -> Result<Vec<u8>> {
        let outcome = from
            .call(self.contract.id(), "ft_transfer")
            .args_json(serde_json::json!({
                "receiver_id": to,
                "amount": amount.to_string(),
            }))
            .deposit(NearToken::from_yoctonear(yocto))
            .max_gas()
            .transact()
            .await?;
        let outcome = outcome.into_result()?;
        Ok(outcome.raw_bytes().unwrap_or_default())
    }

    pub async fn ft_transfer_call(
        &self,
        from: &Account,
        to: &AccountId,
        amount: u128,
        msg: &str,
    ) -> Result<()> {
        from.call(self.contract.id(), "ft_transfer_call")
            .args_json(serde_json::json!({
                "receiver_id": to,
                "amount": amount.to_string(),
                "msg": msg,
            }))
            .deposit(NearToken::from_yoctonear(1))
            .max_gas()
            .transact()
            .await?
            .into_result()?;
        Ok(())
    }

    async fn switch(&self, method: &str, args: serde_json::Value) {
        self.contract
            .call(method)
            .args_json(args)
            .transact()
            .await
            .expect("transaction runs")
            .into_result()
            .expect("switch call succeeds");
    }

    pub async fn set_paused(&self, on: bool) {
        self.switch("set_paused", serde_json::json!({ "paused": on }))
            .await;
    }

    pub async fn set_blocked(&self, account_id: Option<&AccountId>) {
        self.switch(
            "set_blocked",
            serde_json::json!({ "account_id": account_id }),
        )
        .await;
    }

    pub async fn set_panic_in_transfer(&self, on: bool) {
        self.switch("set_panic_in_transfer", serde_json::json!({ "on": on }))
            .await;
    }

    pub async fn set_junk_return(&self, on: bool) {
        self.switch("set_junk_return", serde_json::json!({ "on": on }))
            .await;
    }
}

/// Deploys the `ft_transfer_call` receiver (`contracts/mock-ft-receiver`).
pub async fn deploy_receiver(worker: &Worker<Sandbox>) -> Result<Contract> {
    Ok(worker.dev_deploy(receiver_wasm()).await?)
}
