//! Records NEAR AI's attestation evidence for offline tests and drafts a policy from it.
//!
//! It needs no API key: the report endpoints are public. It does talk to NEAR AI, Intel's PCS and
//! NVIDIA's NRAS directly from this machine, which a user's `nymirad` never does, so run it from a
//! development machine.
//!
//!   cargo run -p nymirad --features mock --example record_evidence -- [OUT_DIR] [MODEL]
//!
//! Writes the two raw reports (with the nonces the fixtures were fetched under), each quote's
//! Intel collateral and the time of recording, and `policy-draft.json` with the deployments seen
//! (unsigned: a person checks them, then signs offline with `policy_tool`). An earlier draft in
//! OUT_DIR is extended, so repeated runs collect the compose hashes of different VMs.

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .unwrap_or_else(|| "crates/nymirad/tests/fixtures/near-ai-recorded".into());
    let model = args.next().unwrap_or_else(|| "z-ai/glm-5.3-flash".into());
    nymirad::record::record(out.into(), model, None, 3).await;
}
