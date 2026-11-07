//! The development vault the `nymirad` and operator sandbox tests share: built with `dev-setup`,
//! one 1 USDC denomination, a 0.05 USDC note fee, a 100 USDC cap, a 7-day exit window.

use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract, Worker};
use serde_json::json;

use crate::build::contract_wasm_with_features;
use crate::mock_usdc::MockUsdc;
use crate::proofs::{canonical_verifying_key, signing_key, Kind};

const ONE: u128 = 1_000_000;
const FEE: u128 = 50_000;

fn point(seed: u64) -> serde_json::Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex_of(&key.x.0)), "y": format!("0x{}", hex_of(&key.y.0)) })
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// What differs between the tests that build this vault.
pub struct DevVault<'a> {
    pub note_ttl_days: u64,
    /// The vault's exit window; the contract takes one day at the least.
    pub exit_delay_secs: u64,
    pub bound_classes: &'a [&'a str],
}

impl Default for DevVault<'_> {
    fn default() -> Self {
        Self {
            note_ttl_days: 30,
            exit_delay_secs: 7 * 86_400,
            bound_classes: &["100000", "500000"],
        }
    }
}

impl DevVault<'_> {
    /// Deploys the vault on a `vault` sub-account of the sandbox root with 50 NEAR, initialises
    /// it (operator key seed 1, clearance key seed 2, the sandbox root as owner) and registers it
    /// and the treasury with the token.
    pub async fn deploy(
        &self,
        worker: &Worker<Sandbox>,
        usdc: &MockUsdc,
        operator_account: &Account,
        treasury: &Account,
    ) -> Contract {
        let vault = worker
            .root_account()
            .unwrap()
            .create_subaccount("vault")
            .initial_balance(NearToken::from_near(50))
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap()
            .deploy(&contract_wasm_with_features(
                "contracts/vault",
                &["dev-setup"],
            ))
            .await
            .unwrap()
            .into_result()
            .unwrap();
        vault
            .call("new")
            .args_json(json!({
                "operator_pk": point(1), "clearance_pk": point(2), "token": usdc.contract.id(),
                "denominations": [ONE.to_string()], "tvl_cap": "100000000",
                "request_vk": b64(&canonical_verifying_key(Kind::Request)),
                "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
                "treasury": treasury.id(), "note_fee": FEE.to_string(),
                "exit_delay_secs": self.exit_delay_secs, "note_ttl": self.note_ttl_days * 86_400,
                "bound_classes": self.bound_classes, "charge_step": "1000", "min_charge": "2000",
                "slot_rate": 1000, "owner": worker.root_account().unwrap().id(),
                "operator_account": operator_account.id(),
            }))
            .max_gas()
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
        usdc.register_id(vault.id()).await;
        usdc.register(treasury).await;
        vault
    }
}
