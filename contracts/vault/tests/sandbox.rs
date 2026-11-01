//! The stub, built with the pinned rustc through cargo-near, deploys and runs
//! on the NEAR sandbox.

use std::path::Path;
use std::process::Command;

/// `NYMIRA_VAULT_WASM` points the test at a prebuilt wasm (the reproducible build).
fn build_wasm() -> Vec<u8> {
    if let Ok(path) = std::env::var("NYMIRA_VAULT_WASM") {
        return std::fs::read(path).expect("NYMIRA_VAULT_WASM is readable");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("cargo")
        .args(["near", "build", "non-reproducible-wasm", "--no-abi", "--locked"])
        .current_dir(root)
        .status()
        .expect("cargo-near 0.22.0 is installed");
    assert!(status.success(), "cargo near build failed");
    std::fs::read(root.join("../../target/near/nymira_vault/nymira_vault.wasm")).unwrap()
}

#[tokio::test]
async fn stub_deploys_and_runs() -> Result<(), Box<dyn std::error::Error>> {
    let wasm = build_wasm();
    let worker = near_workspaces::sandbox().await?;
    let contract = worker.dev_deploy(&wasm).await?;
    let version: u32 = contract.view("stub_version").await?.json()?;
    assert_eq!(version, 1);
    Ok(())
}
