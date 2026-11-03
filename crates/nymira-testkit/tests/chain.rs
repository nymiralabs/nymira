//! Self-test: the sandbox starts at the mainnet and at the testnet nearcore release, and the
//! mock token behaves the same on both.

use nymira_testkit::chain::{sandbox_for, Chain, MAINNET_NEARCORE, TESTNET_NEARCORE};
use nymira_testkit::mock_usdc::MockUsdc;

#[test]
fn releases_are_distinct() {
    assert_ne!(MAINNET_NEARCORE, TESTNET_NEARCORE);
    assert_eq!(Chain::Mainnet.nearcore_version(), "2.13.4");
}

/// What the node itself reports through its `status` call: `(version, protocol_version)`.
async fn reported(
    worker: &near_workspaces::Worker<near_workspaces::network::Sandbox>,
) -> (String, u64) {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": "1", "method": "status", "params": []});
    let answer: serde_json::Value = reqwest::Client::new()
        .post(worker.rpc_addr())
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let version = answer["result"]["version"]["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let protocol = answer["result"]["protocol_version"].as_u64().unwrap();
    (version, protocol)
}

#[tokio::test]
async fn each_chain_runs_the_nearcore_release_it_names() {
    for (chain, protocol) in [(Chain::Mainnet, 86), (Chain::Testnet, 88)] {
        let worker = sandbox_for(chain).await.unwrap();
        let (version, reported_protocol) = reported(&worker).await;
        assert!(
            version.starts_with(chain.nearcore_version()),
            "{chain:?}: the node reports {version}"
        );
        assert_eq!(reported_protocol, protocol, "{chain:?}");
    }
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
