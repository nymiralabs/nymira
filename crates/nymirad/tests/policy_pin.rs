//! The policy shipped in `pins/` verifies under the signer key pinned next to it, so a build
//! never carries a pair that cannot load.

use nymirad::attest::Policy;

#[test]
fn the_shipped_policy_loads_under_the_pinned_signer() {
    let signer: [u8; 32] = hex::decode(include_str!("../pins/policy-signer.pub").trim())
        .unwrap()
        .try_into()
        .unwrap();
    let policy = Policy::load(
        include_bytes!("../pins/attestation-policy.json"),
        &signer,
        2,
    )
    .expect("signed by the pinned key");
    assert_eq!(policy.accepted_tcb_statuses, ["UpToDate"]);
    for role in ["model", "gateway"] {
        assert!(
            policy.deployments.iter().any(|d| d.role == role),
            "no {role} deployment"
        );
    }
}

/// Every model report the team recorded verifies against the policy that ships: the compose hash
/// each shows is listed. (Version 1 listed the first recording's hash only, and refused the
/// deployment that produced the second.)
#[test]
fn every_recorded_model_report_is_accepted_by_the_shipped_policy() {
    let policy: Policy =
        serde_json::from_slice(include_bytes!("../pins/attestation-policy.json")).unwrap();
    let model = policy
        .deployments
        .iter()
        .find(|d| d.role == "model")
        .expect("a model deployment");
    let reports = [
        include_str!("fixtures/near-ai-recorded/model-report.json"),
        include_str!("fixtures/near-ai-recorded/chat/model-report.json"),
    ];
    for text in reports {
        let report: serde_json::Value = serde_json::from_str(text).unwrap();
        let log = &report["model_attestations"][0]["event_log"];
        let log: serde_json::Value = match log {
            serde_json::Value::String(s) => serde_json::from_str(s).unwrap(),
            other => other.clone(),
        };
        let shown = log
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event"] == "compose-hash")
            .and_then(|e| e["event_payload"].as_str())
            .expect("a compose-hash event")
            .to_string();
        assert!(
            model.compose_hashes.contains(&shown),
            "the shipped policy refuses a deployment we recorded: {shown}"
        );
    }
}
