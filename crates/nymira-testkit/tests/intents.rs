//! Self-test of the open-source Verifier v0.4.2 fixture.

use nymira_testkit::intents::{deploy_verifier, verifier_wasm, VERIFIER_SHA256};

#[test]
fn fixture_matches_the_published_release_digest() {
    verifier_wasm();
    assert_eq!(VERIFIER_SHA256.len(), 64);
}

#[tokio::test]
async fn the_open_source_verifier_deploys_and_answers_views() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let verifier = deploy_verifier(&worker, "intents", "wrap.test.near")
        .await
        .unwrap();
    let fee: serde_json::Value = verifier.view("fee").await.unwrap().json().unwrap();
    assert_eq!(fee, 0);
    let supply: serde_json::Value = verifier
        .view("mt_supply")
        .args_json(serde_json::json!({ "token_id": "nep141:usdc.test.near" }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(supply, "0", "no deposits yet");
}

/// Deposit USDC into the Verifier, then `ft_withdraw(..., msg)`: the Verifier's
/// `ft_transfer_call` reaches the receiver (standing in for the vault) with the Verifier as
/// `sender_id` and the given `msg`.
#[tokio::test]
async fn deposit_then_withdraw_reaches_the_receiver_with_the_verifier_as_sender() {
    use near_workspaces::types::NearToken;
    use nymira_testkit::mock_usdc::{deploy_receiver, MockUsdc};

    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let verifier = deploy_verifier(&worker, "intents", "wrap.test.near")
        .await
        .unwrap();
    let receiver = deploy_receiver(&worker).await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    usdc.register(&alice).await;
    usdc.register_id(verifier.id()).await;
    usdc.register_id(receiver.id()).await;
    usdc.mint(alice.id(), 5_000_000).await;

    // Deposit: ft_transfer_call into the Verifier, credited to alice.
    usdc.ft_transfer_call(
        &alice,
        verifier.id(),
        2_000_000,
        &format!("{{\"receiver_id\":\"{}\"}}", alice.id()),
    )
    .await
    .unwrap();
    assert_eq!(usdc.balance_of(verifier.id()).await, 2_000_000);
    let token_id = format!("nep141:{}", usdc.contract.id());
    let credited: String = verifier
        .view("mt_balance_of")
        .args_json(serde_json::json!({ "account_id": alice.id(), "token_id": token_id }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(credited, "2000000");

    // Withdraw to the receiver with a msg.
    alice
        .call(verifier.id(), "ft_withdraw")
        .args_json(serde_json::json!({
            "token": usdc.contract.id(),
            "receiver_id": receiver.id(),
            "amount": "1500000",
            "msg": "{\"withdraw_to\":\"bob.test.near\"}",
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let seen: serde_json::Value = receiver.view("last_call").await.unwrap().json().unwrap();
    assert_eq!(seen["sender_id"], verifier.id().to_string());
    assert_eq!(seen["predecessor"], usdc.contract.id().to_string());
    assert_eq!(seen["amount"], "1500000");
    assert_eq!(seen["msg"], "{\"withdraw_to\":\"bob.test.near\"}");
    assert_eq!(usdc.balance_of(receiver.id()).await, 1_500_000);
}
