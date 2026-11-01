//! Self-test of the mainnet-parity fixture: the real mainnet USDC wasm runs in the sandbox,
//! and its NEP-145 registration rules agree with the mock token's.

use near_workspaces::types::NearToken;
use nymira_testkit::mainnet_usdc;
use nymira_testkit::mock_usdc::{MockUsdc, MIN_STORAGE_DEPOSIT};

#[test]
fn fixture_hashes_are_the_recorded_ones() {
    mainnet_usdc::wasm();
    assert_ne!(
        mainnet_usdc::MAINNET_CODE_HASH,
        mainnet_usdc::TESTNET_CODE_HASH
    );
}

#[tokio::test]
async fn real_usdc_runs_and_has_six_decimals_and_the_published_storage_minimum() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let real = mainnet_usdc::deploy(&worker).await.unwrap();
    let metadata: serde_json::Value = real.view("ft_metadata").await.unwrap().json().unwrap();
    assert_eq!(metadata["decimals"], 6);
    let bounds: serde_json::Value = real
        .view("storage_balance_bounds")
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(bounds["min"], MIN_STORAGE_DEPOSIT.to_string());
}

/// The same registration steps on the real contract and on the mock give the same answers.
#[tokio::test]
async fn registration_rules_match_the_mock() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let real = mainnet_usdc::deploy(&worker).await.unwrap();
    let mock = MockUsdc::deploy(&worker).await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    let bob = worker.dev_create_account().await.unwrap();

    async fn registered(c: &near_workspaces::Contract, id: &near_workspaces::AccountId) -> bool {
        let v: serde_json::Value = c
            .view("storage_balance_of")
            .args_json(serde_json::json!({ "account_id": id }))
            .await
            .unwrap()
            .json()
            .unwrap();
        !v.is_null()
    }
    async fn deposit(
        c: &near_workspaces::Contract,
        who: &near_workspaces::Account,
        yocto: u128,
        registration_only: bool,
    ) -> bool {
        who.call(c.id(), "storage_deposit")
            .args_json(serde_json::json!({ "registration_only": registration_only }))
            .deposit(NearToken::from_yoctonear(yocto))
            .max_gas()
            .transact()
            .await
            .unwrap()
            .is_success()
    }

    // Below the minimum: both refuse and register nothing.
    for c in [&real, &mock.contract] {
        assert!(!deposit(c, &alice, MIN_STORAGE_DEPOSIT - 1, true).await);
        assert!(!registered(c, alice.id()).await);
    }
    // Exactly the minimum registers; an excess registers too.
    for c in [&real, &mock.contract] {
        assert!(deposit(c, &alice, MIN_STORAGE_DEPOSIT, true).await);
        assert!(registered(c, alice.id()).await);
        assert!(deposit(c, &bob, 10 * MIN_STORAGE_DEPOSIT, true).await);
        assert!(registered(c, bob.id()).await);
    }
    // Registering again succeeds and changes nothing.
    for c in [&real, &mock.contract] {
        assert!(deposit(c, &alice, MIN_STORAGE_DEPOSIT, false).await);
        assert!(registered(c, alice.id()).await);
    }
    // ft_transfer to an unregistered account fails on both (nothing to move, but the
    // registration check comes first on the real token; the mock matches).
    let carol = worker.dev_create_account().await.unwrap();
    for c in [&real, &mock.contract] {
        let outcome = alice
            .call(c.id(), "ft_transfer")
            .args_json(serde_json::json!({ "receiver_id": carol.id(), "amount": "1" }))
            .deposit(NearToken::from_yoctonear(1))
            .max_gas()
            .transact()
            .await
            .unwrap();
        assert!(outcome.is_failure());
    }
}
