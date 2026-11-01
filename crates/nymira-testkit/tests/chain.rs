//! Self-test: the sandbox starts at the mainnet and at the testnet nearcore release, and the
//! mock token behaves the same on both.

use nymira_testkit::chain::{sandbox_for, Chain, MAINNET_NEARCORE, TESTNET_NEARCORE};
use nymira_testkit::mock_usdc::MockUsdc;

#[test]
fn releases_are_distinct() {
    assert_ne!(MAINNET_NEARCORE, TESTNET_NEARCORE);
    assert_eq!(Chain::Mainnet.nearcore_version(), "2.13.4");
}

#[tokio::test]
async fn mock_token_runs_at_both_releases() {
    for chain in [Chain::Mainnet, Chain::Testnet] {
        let worker = sandbox_for(chain).await.unwrap();
        let usdc = MockUsdc::deploy(&worker).await.unwrap();
        let alice = worker.dev_create_account().await.unwrap();
        usdc.register(&alice).await;
        usdc.mint(alice.id(), 7).await;
        assert_eq!(usdc.balance_of(alice.id()).await, 7, "{chain:?}");
    }
}
