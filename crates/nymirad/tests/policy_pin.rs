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
        1,
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
