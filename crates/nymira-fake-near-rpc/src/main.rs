//! Runs one fake NEAR RPC. Usage: `nymira-fake-near-rpc [--addr HOST:PORT] [--chain-id ID]`.
//! `NYMIRA_FAKE_RPC_ADDR` sets the address when the flag is absent. Default `127.0.0.1:0`.

use nymira_fake_near_rpc::{Config, FakeRpc};

#[tokio::main]
async fn main() {
    let mut config = Config::default();
    if let Ok(a) = std::env::var("NYMIRA_FAKE_RPC_ADDR") {
        config.addr = a.parse().expect("NYMIRA_FAKE_RPC_ADDR must be HOST:PORT");
    }
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--addr" => config.addr = value.parse().expect("--addr must be HOST:PORT"),
            "--chain-id" => config.chain_id = value,
            other => panic!("unknown flag {other}"),
        }
    }
    let rpc = FakeRpc::bind(config).await.expect("bind");
    println!("listening on {}", rpc.url());
    std::future::pending::<()>().await;
}
