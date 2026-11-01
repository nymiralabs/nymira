//! Real fixture proofs verified by `nymira-verifier` on the NEAR sandbox:
//! gas, and what the host enforces.

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn build_wasm() -> Vec<u8> {
    let status = Command::new("cargo")
        .args([
            "near",
            "build",
            "non-reproducible-wasm",
            "--no-abi",
            "--locked",
        ])
        .current_dir(root())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(root().join("../../target/near/verifier_probe/verifier_probe.wasm")).unwrap()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn hex_bytes(value: &serde_json::Value) -> Vec<u8> {
    hex::decode(value.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

fn canonical_vk(file: &str) -> Vec<u8> {
    let bytes = std::fs::read(
        root()
            .join("../../vendor/zkapi/protocol/setup/v2-concession-dev")
            .join(file),
    )
    .unwrap();
    // Header is `<circuit>/<setup>/<kind>\0`; the canonical key follows it.
    let end = bytes.iter().position(|b| *b == 0).unwrap();
    bytes[end + 1..].to_vec()
}

fn gas_line(logs: &[&str]) -> String {
    logs.iter()
        .find(|l| l.starts_with("gas "))
        .map(|l| l.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn fixtures_verify_on_the_sandbox_and_gas_is_measured(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(
        root().join("../../crates/nymira-verifier/tests/fixtures/zkapi-dev-fixture.json"),
    )?)?;
    let worker = near_workspaces::sandbox().await?;
    let contract = worker.dev_deploy(&build_wasm()).await?;
    for (case, vk_file) in [("request", "request.vk"), ("withdrawal", "withdrawal.vk")] {
        let words: Vec<String> = hex_bytes(&fixture[case]["inputs_abi"])
            .chunks(32)
            .map(b64)
            .collect();
        let proof = hex_bytes(&fixture[case]["proof_hex"]);
        let call = |proof: &[u8]| {
            contract
                .call("verify")
                .args_json(serde_json::json!({
                    "vk": b64(&canonical_vk(vk_file)),
                    "inputs": words,
                    "proof": b64(proof),
                }))
                .max_gas()
                .transact()
        };
        let outcome = call(&proof).await?;
        assert!(outcome.is_success(), "{case}: {:?}", outcome.failures());
        println!(
            "SANDBOX {case}: total_gas_burnt={:.2} Tgas, {}",
            outcome.total_gas_burnt.as_gas() as f64 / 1e12,
            gas_line(&outcome.logs())
        );
        assert!(outcome.json::<bool>()?, "{case} must verify");

        // A flipped proof bit is off the curve or fails the pairing: never accepted.
        let mut tampered = proof.clone();
        tampered[10] ^= 1;
        let outcome = call(&tampered).await?;
        let accepted = outcome.is_success() && outcome.clone().json::<bool>().unwrap_or(false);
        assert!(!accepted, "{case}: tampered proof accepted");
    }
    Ok(())
}

/// The real host refuses a G2 point on the twist outside the order-r subgroup, so
/// `nymira-verifier` does not multiply B by the group order.
#[tokio::test]
async fn the_host_enforces_the_g2_subgroup_check() -> Result<(), Box<dyn std::error::Error>> {
    use ark_bn254::{Fq, Fq2, G2Affine};
    use ark_ff::{BigInteger, PrimeField};

    let bad = (1u64..200)
        .filter_map(|i| {
            G2Affine::get_point_from_x_unchecked(Fq2::new(Fq::from(i), Fq::from(1u64)), false)
        })
        .find(|p| p.is_on_curve() && !p.is_in_correct_subgroup_assuming_on_curve())
        .expect("a twist point outside the subgroup");
    let be = |f: &Fq| {
        let mut v = f.into_bigint().to_bytes_be();
        while v.len() < 32 {
            v.insert(0, 0);
        }
        v
    };
    let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(
        root().join("../../crates/nymira-verifier/tests/fixtures/zkapi-dev-fixture.json"),
    )?)?;
    let mut proof = hex_bytes(&fixture["request"]["proof_hex"]);
    proof[64..192]
        .copy_from_slice(&[be(&bad.x.c0), be(&bad.x.c1), be(&bad.y.c0), be(&bad.y.c1)].concat());
    let words: Vec<String> = hex_bytes(&fixture["request"]["inputs_abi"])
        .chunks(32)
        .map(b64)
        .collect();
    let worker = near_workspaces::sandbox().await?;
    let contract = worker.dev_deploy(&build_wasm()).await?;
    let outcome = contract
        .call("verify")
        .args_json(serde_json::json!({
            "vk": b64(&canonical_vk("request.vk")),
            "inputs": words,
            "proof": b64(&proof),
        }))
        .max_gas()
        .transact()
        .await?;
    let failure = format!("{:?}", outcome.failures());
    println!(
        "SANDBOX g2 subgroup: success={} {}",
        outcome.is_success(),
        &failure[..failure.len().min(300)]
    );
    assert!(
        outcome.is_failure(),
        "host accepted a G2 point outside the subgroup"
    );
    Ok(())
}
