//! Builds test contracts to wasm with the pinned toolchain through cargo-near,
//! once per test process.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root (`crates/nymira-testkit/../..`).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where cargo-near writes its wasm. It follows `CARGO_TARGET_DIR`, as cargo does. A relative
/// value may be read against the build's working directory or the workspace root, so both are
/// offered and the caller takes the one that exists.
fn near_dirs(root: &Path, manifest: &Path, custom: Option<&Path>) -> Vec<PathBuf> {
    match custom {
        None => vec![root.join("target/near")],
        Some(dir) if dir.is_absolute() => vec![dir.join("near")],
        Some(dir) => vec![manifest.join(dir).join("near"), root.join(dir).join("near")],
    }
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
    let custom = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    let mut last = None;
    for place in near_dirs(&root, &manifest, custom.as_deref()) {
        let wasm = place.join(&name).join(format!("{name}.wasm"));
        match std::fs::read(&wasm) {
            Ok(bytes) => return bytes,
            Err(e) => last = Some((wasm, e)),
        }
    }
    let (wasm, e) = last.expect("at least one wasm place");
    panic!(
        "built wasm for {dir} is readable at {}: {e}",
        wasm.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_custom_target_dir_holds_the_wasm() {
        let root = Path::new("/workspace");
        let manifest = root.join("contracts/mock-usdc");
        assert_eq!(near_dirs(root, &manifest, None), vec![PathBuf::from("/workspace/target/near")]);
        assert_eq!(
            near_dirs(root, &manifest, Some(Path::new("/tmp/build"))),
            vec![PathBuf::from("/tmp/build/near")]
        );
        assert_eq!(
            near_dirs(root, &manifest, Some(Path::new("out"))),
            vec![
                manifest.join("out/near"),
                PathBuf::from("/workspace/out/near"),
            ]
        );
    }
}
