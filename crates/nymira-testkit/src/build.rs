//! Builds test contracts to wasm with the pinned toolchain through cargo-near,
//! once per test process.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root (`crates/nymira-testkit/../..`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the contract crate at `dir` (relative to the workspace root, for
/// example `contracts/mock-usdc`) and returns its wasm. cargo-near 0.22.0 must
/// be installed. Concurrent callers serialize on cargo's own build lock.
pub fn contract_wasm(dir: &str) -> Vec<u8> {
    let root = workspace_root();
    let manifest = root.join(dir);
    let name = manifest
        .file_name()
        .expect("contract directory has a name")
        .to_string_lossy()
        .replace('-', "_");
    let status = Command::new("cargo")
        .args([
            "near",
            "build",
            "non-reproducible-wasm",
            "--no-abi",
            "--locked",
        ])
        .current_dir(&manifest)
        .status()
        .expect("cargo-near 0.22.0 is installed");
    assert!(status.success(), "cargo near build failed for {dir}");
    std::fs::read(
        root.join("target/near")
            .join(&name)
            .join(format!("{name}.wasm")),
    )
    .unwrap_or_else(|e| panic!("built wasm for {dir} is readable: {e}"))
}
