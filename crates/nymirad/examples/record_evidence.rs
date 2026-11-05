//! Records NEAR AI's attestation evidence for offline tests and drafts a policy from it.
//!
//! It needs no API key: the report endpoints are public. It does talk to NEAR AI, Intel's PCS and
//! NVIDIA's NRAS directly from this machine, which a user's `nymirad` never does, so run it from a
//! development machine.
//!
//!   cargo run -p nymirad --features mock --example record_evidence -- [OUT_DIR] [MODEL] [--gateway-wildcard]
//!
//! Writes the two raw reports (with the nonces the fixtures were fetched under), each quote's
//! Intel collateral and the time of recording, and `policy-draft.json` with the deployments seen
//! (unsigned: a person checks them, then signs offline with `policy_tool`). An earlier draft in
//! OUT_DIR is extended, so repeated runs collect the compose hashes of different VMs.
//! `--gateway-wildcard` writes the gateway entry as accepting any compose hash, for a gateway
//! that NEAR AI redeploys with a new compose file each time; the model entry keeps its hashes.

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let wildcard = args
        .iter()
        .position(|a| a == "--gateway-wildcard")
        .map(|at| args.remove(at))
        .is_some();
    let mut args = args.into_iter();
    let out = args
        .next()
        .unwrap_or_else(|| "crates/nymirad/tests/fixtures/near-ai-recorded".into());
    let model = args.next().unwrap_or_else(|| "z-ai/glm-5.3-flash".into());
    let out: std::path::PathBuf = out.into();
    nymirad::record::record(out.clone(), model, None, 3).await;
    if wildcard {
        let path = out.join("policy-draft.json");
        let mut draft: nymirad::attest::Policy =
            serde_json::from_slice(&std::fs::read(&path).expect("draft")).expect("draft parses");
        nymirad::record::gateway_wildcard(&mut draft.deployments);
        std::fs::write(&path, serde_json::to_vec_pretty(&draft).unwrap()).unwrap();
        println!("marked the gateway entries as accepting any compose hash");
    }
}
