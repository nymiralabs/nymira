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
/// be installed.
pub fn contract_wasm(dir: &str) -> Vec<u8> {
    contract_wasm_with_features(dir, &[])
}

/// [`contract_wasm`] with cargo features (for example the vault's `dev-setup`).
/// The same crate built with other features lands at the same path, so builds
/// of one process serialize here.
pub fn contract_wasm_with_features(dir: &str, features: &[&str]) -> Vec<u8> {
    static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let root = workspace_root();
    let manifest = root.join(dir);
    // The package name is the first `name = "..."` of the manifest.
    let name = std::fs::read_to_string(manifest.join("Cargo.toml"))
        .expect("contract manifest is readable")
        .lines()
        .find_map(|line| line.strip_prefix("name = \""))
        .and_then(|rest| rest.split('"').next().map(str::to_owned))
        .expect("manifest names the package")
        .replace('-', "_");
    let mut command = Command::new("cargo");
    command.args([
        "near",
        "build",
        "non-reproducible-wasm",
        "--no-abi",
        "--locked",
    ]);
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    let status = command
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
