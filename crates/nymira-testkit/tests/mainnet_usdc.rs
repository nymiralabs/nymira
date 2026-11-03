//! Self-test of the mainnet-parity fixture: the real mainnet USDC wasm runs in the sandbox,
//! and its NEP-145 registration rules agree with the mock token's.

use near_workspaces::types::NearToken;
use nymira_testkit::mainnet_usdc;
use nymira_testkit::mock_usdc::{MockUsdc, MIN_STORAGE_DEPOSIT};

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
}

/// Balances move the same way on both tokens, and the NEAR refunds the vault's payout path relies
/// on match: an excess of a registration deposit and a deposit for an account that is already
/// registered both come back (the cost is gas plus at most the minimum, never the whole deposit).
#[tokio::test]
async fn transfers_and_storage_refunds_match_the_mock() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let real = MockUsdc::deploy_real(&worker).await.unwrap();
    let mock = MockUsdc::deploy(&worker).await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    let carol = worker.dev_create_account().await.unwrap();

    async fn transfer(
        token: &MockUsdc,
        from: &near_workspaces::Account,
        to: &near_workspaces::AccountId,
        amount: u128,
    ) -> bool {
        from.call(token.contract.id(), "ft_transfer")
            .args_json(serde_json::json!({ "receiver_id": to, "amount": amount.to_string() }))
            .deposit(NearToken::from_yoctonear(1))
            .max_gas()
            .transact()
            .await
            .unwrap()
            .is_success()
    }
    /// What `who` paid in NEAR for one `storage_deposit` of `yocto` (gas included).
    async fn cost_of_deposit(
        worker: &near_workspaces::Worker<near_workspaces::network::Sandbox>,
        token: &MockUsdc,
        who: &near_workspaces::Account,
        yocto: u128,
    ) -> u128 {
        // Earlier calls refund unused gas in later blocks; let those land first.
        worker.fast_forward(3).await.unwrap();
        let before = who.view_account().await.unwrap().balance.as_yoctonear();
        who.call(token.contract.id(), "storage_deposit")
            .args_json(serde_json::json!({ "registration_only": true }))
            .deposit(NearToken::from_yoctonear(yocto))
            .max_gas()
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
        // The refund is a receipt of its own.
        worker.fast_forward(3).await.unwrap();
        let after = who.view_account().await.unwrap().balance.as_yoctonear();
        before - after
    }

    for token in [&real, &mock] {
        let label = if token.is_real() { "real" } else { "mock" };
        token.register(&alice).await;
        token.mint(alice.id(), 10).await;
        assert_eq!(token.balance_of(alice.id()).await, 10, "{label}");
        // An unregistered receiver is refused and nothing moves (alice does hold the tokens).
        assert!(!transfer(token, &alice, carol.id(), 3).await, "{label}");
        assert_eq!(token.balance_of(alice.id()).await, 10, "{label}");
        token.register(&carol).await;
        assert!(transfer(token, &alice, carol.id(), 3).await, "{label}");
        assert_eq!(token.balance_of(alice.id()).await, 7, "{label}");
        assert_eq!(token.balance_of(carol.id()).await, 3, "{label}");
        // Already registered: the whole deposit comes back, so the cost is gas alone.
        let again = cost_of_deposit(&worker, token, &alice, 10 * MIN_STORAGE_DEPOSIT).await;
        assert!(again < 5 * MIN_STORAGE_DEPOSIT, "{label}: paid {again}");
        // A new account with an excess: the minimum is kept, the rest comes back.
        let dave = worker.dev_create_account().await.unwrap();
        let fresh = cost_of_deposit(&worker, token, &dave, 10 * MIN_STORAGE_DEPOSIT).await;
        assert!(fresh >= MIN_STORAGE_DEPOSIT, "{label}: paid {fresh}");
        assert!(fresh < 10 * MIN_STORAGE_DEPOSIT, "{label}: paid {fresh}");
    }
}
