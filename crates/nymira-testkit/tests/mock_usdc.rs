//! Self-test of the mock USDC token: NEP-141 and NEP-145 with the registration
//! rules the vault relies on, and the failure switches.

use near_workspaces::types::NearToken;
use near_workspaces::{Account, Worker};
use nymira_testkit::mock_usdc::{MockUsdc, MIN_STORAGE_DEPOSIT};

/// Gas refunds arrive a few blocks after a transaction: let them land before reading a balance.
async fn settled_balance(
    worker: &Worker<near_workspaces::network::Sandbox>,
    account: &Account,
) -> u128 {
    worker.fast_forward(5).await.unwrap();
    account.view_account().await.unwrap().balance.as_yoctonear()
}

async fn setup() -> (
    Worker<near_workspaces::network::Sandbox>,
    MockUsdc,
    Account,
    Account,
) {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    let bob = worker.dev_create_account().await.unwrap();
    (worker, usdc, alice, bob)
}

#[tokio::test]
async fn registration_only_deposit_registers_and_refunds_the_excess() {
    let (w, usdc, alice, _bob) = setup().await;
    assert!(!usdc.is_registered(alice.id()).await);
    let before = settled_balance(&w, &alice).await;
    // Deposit 1 NEAR with registration_only: only the minimum is kept.
    usdc.storage_deposit(&alice, None, true, NearToken::from_near(1))
        .await
        .unwrap();
    assert!(usdc.is_registered(alice.id()).await);
    let after = settled_balance(&w, &alice).await;
    let spent = before - after;
    assert!(
        spent >= MIN_STORAGE_DEPOSIT,
        "kept at least the minimum: {spent}"
    );
    assert!(
        spent < MIN_STORAGE_DEPOSIT + 10u128.pow(22),
        "excess refunded: {spent}"
    );
}

#[tokio::test]
async fn deposit_below_the_minimum_fails() {
    let (_w, usdc, alice, _bob) = setup().await;
    let result = usdc
        .storage_deposit(
            &alice,
            None,
            true,
            NearToken::from_yoctonear(MIN_STORAGE_DEPOSIT - 1),
        )
        .await;
    assert!(result.is_err());
    assert!(!usdc.is_registered(alice.id()).await);
}

#[tokio::test]
async fn deposit_for_an_already_registered_account_is_refunded_in_full() {
    let (w, usdc, alice, _bob) = setup().await;
    usdc.register(&alice).await;
    let before = settled_balance(&w, &alice).await;
    usdc.storage_deposit(&alice, None, false, NearToken::from_near(1))
        .await
        .unwrap();
    let after = settled_balance(&w, &alice).await;
    // Only gas moved, not the 1 NEAR.
    let diff = before as i128 - after as i128;
    assert!(diff.abs() < 10i128.pow(22), "balance moved by {diff}");
}

#[tokio::test]
async fn bounds_are_the_published_minimum() {
    let (_w, usdc, _alice, _bob) = setup().await;
    assert_eq!(usdc.storage_balance_bounds_min().await, MIN_STORAGE_DEPOSIT);
    assert_eq!(MIN_STORAGE_DEPOSIT, 1_250_000_000_000_000_000_000);
}

#[tokio::test]
async fn transfer_moves_balance_between_registered_accounts() {
    let (_w, usdc, alice, bob) = setup().await;
    usdc.register(&alice).await;
    usdc.register(&bob).await;
    usdc.mint(alice.id(), 5_000_000).await;
    usdc.ft_transfer(&alice, bob.id(), 2_000_000).await.unwrap();
    assert_eq!(usdc.balance_of(alice.id()).await, 3_000_000);
    assert_eq!(usdc.balance_of(bob.id()).await, 2_000_000);
}

#[tokio::test]
async fn transfer_to_an_unregistered_account_fails_and_moves_nothing() {
    let (_w, usdc, alice, bob) = setup().await;
    usdc.register(&alice).await;
    usdc.mint(alice.id(), 1_000_000).await;
    assert!(usdc.ft_transfer(&alice, bob.id(), 1).await.is_err());
    assert_eq!(usdc.balance_of(alice.id()).await, 1_000_000);
}

#[tokio::test]
async fn transfer_needs_one_yocto_and_enough_balance() {
    let (_w, usdc, alice, bob) = setup().await;
    usdc.register(&alice).await;
    usdc.register(&bob).await;
    usdc.mint(alice.id(), 10).await;
    assert!(usdc.ft_transfer(&alice, bob.id(), 11).await.is_err());
    assert!(usdc
        .ft_transfer_with_deposit(&alice, bob.id(), 1, 0)
        .await
        .is_err());
    assert!(usdc
        .ft_transfer_with_deposit(&alice, bob.id(), 1, 2)
        .await
        .is_err());
}

#[tokio::test]
async fn switches_pause_blocklist_panic_and_junk_return() {
    let (_w, usdc, alice, bob) = setup().await;
    usdc.register(&alice).await;
    usdc.register(&bob).await;
    usdc.mint(alice.id(), 100).await;

    usdc.set_paused(true).await;
    assert!(usdc.ft_transfer(&alice, bob.id(), 1).await.is_err());
    usdc.set_paused(false).await;
    usdc.ft_transfer(&alice, bob.id(), 1).await.unwrap();

    // Blocklisting one account stops it as sender and as receiver, nobody else.
    usdc.set_blocked(Some(bob.id())).await;
    assert!(usdc.ft_transfer(&alice, bob.id(), 1).await.is_err());
    usdc.mint(bob.id(), 5).await;
    assert!(usdc.ft_transfer(&bob, alice.id(), 1).await.is_err());
    usdc.set_blocked(None).await;
    usdc.ft_transfer(&bob, alice.id(), 1).await.unwrap();

    usdc.set_panic_in_transfer(true).await;
    assert!(usdc.ft_transfer(&alice, bob.id(), 1).await.is_err());
    usdc.set_panic_in_transfer(false).await;

    // Junk return bytes on a successful ft_transfer (the standard returns nothing).
    let normal = usdc.ft_transfer(&alice, bob.id(), 1).await.unwrap();
    assert!(normal.is_empty());
    usdc.set_junk_return(true).await;
    let junk = usdc.ft_transfer(&alice, bob.id(), 1).await.unwrap();
    assert!(!junk.is_empty(), "successful transfer returned junk bytes");
}

#[tokio::test]
async fn transfer_call_hands_the_receiver_the_sender_amount_and_msg_and_refunds_the_unused_part() {
    let (worker, usdc, alice, _bob) = setup().await;
    let receiver = nymira_testkit::mock_usdc::deploy_receiver(&worker)
        .await
        .unwrap();
    usdc.register(&alice).await;
    usdc.register_id(receiver.id()).await;
    usdc.mint(alice.id(), 1_000).await;
    // The receiver keeps 700 and returns 300 as unused.
    receiver
        .call("set_unused")
        .args_json(serde_json::json!({"amount": "300"}))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    usdc.ft_transfer_call(&alice, receiver.id(), 1_000, "hello")
        .await
        .unwrap();
    assert_eq!(usdc.balance_of(receiver.id()).await, 700);
    assert_eq!(usdc.balance_of(alice.id()).await, 300);
    let seen: serde_json::Value = receiver.view("last_call").await.unwrap().json().unwrap();
    assert_eq!(seen["sender_id"], alice.id().to_string());
    assert_eq!(seen["amount"], "1000");
    assert_eq!(seen["msg"], "hello");
    assert_eq!(seen["predecessor"], usdc.contract.id().to_string());
}
