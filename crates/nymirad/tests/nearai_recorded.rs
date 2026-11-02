//! `NearAiAttestor` against NEAR AI's evidence recorded on 2026-11-02 (`tests/fixtures/near-ai-recorded`,
//! made by `cargo run -p nymirad --example record_evidence`). The Intel quotes verify offline
//! against their recorded collateral at the recorded time, so the quote signature, the TCB
//! status, the nonce and the report-data binding are real checks, not stubs. The GPU step replays
//! NVIDIA's recorded "verified" (its service signs a token that expires in minutes); the live
//! check ran when the evidence was recorded.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use nearai_inference_sdk::{
    CompletionSignatureKind, DeploymentProvenanceStatus, GatewayTlsBinding, GpuEvidenceStatus,
    GpuEvidenceVerifier, MeasuredDeployment, RuntimeMeasurements, SigningAlgo, SigningIdentity,
    TcbStatus, VerificationError, VerifiedAttestationEvidence, VerifiedGatewayAttestation,
    VerifiedModelAttestation,
};
use nymirad::attest::{
    Attestor, Deployment, ModelIdentity, Policy, PolicyError, ReportRequest, Trust,
};
use nymirad::nearai::{Collateral, NearAiAttestor, Replay};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MODEL: &str = "z-ai/glm-5.3-flash";

fn fixture(name: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/near-ai-recorded");
    std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn meta() -> Value {
    serde_json::from_slice(&fixture("meta.json")).unwrap()
}

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn deployments() -> Vec<Deployment> {
    let draft: Policy = serde_json::from_slice(&fixture("policy-draft.json")).unwrap();
    draft.deployments
}

fn policy(deployments: Vec<Deployment>) -> Policy {
    Policy {
        version: 1,
        attestation_root_pk: String::new(),
        accepted_measurements: vec![],
        deployments,
        accepted_tcb_statuses: vec!["UpToDate".into()],
        signature: String::new(),
    }
    .sign(&signer())
}

/// NVIDIA's recorded verdict, replayed.
struct RecordedGpu;

#[async_trait]
impl GpuEvidenceVerifier for RecordedGpu {
    async fn verify(&self, _payload: &str) -> Result<(), VerificationError> {
        Ok(())
    }
}

type Bodies = Arc<Mutex<(String, String)>>;

async fn serve(model_report: String, gateway_report: String) -> String {
    async fn report(
        State(b): State<Bodies>,
        Query(q): Query<std::collections::HashMap<String, String>>,
    ) -> String {
        let b = b.lock().unwrap();
        if q.contains_key("model") {
            b.0.clone()
        } else {
            b.1.clone()
        }
    }
    let app = Router::new()
        .route("/v1/attestation/report", get(report))
        .with_state(Arc::new(Mutex::new((model_report, gateway_report))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn collateral(name: &str, at_secs: u64) -> Collateral {
    Collateral::Recorded {
        collateral: Box::new(serde_json::from_slice(&fixture(name)).unwrap()),
        at_secs,
    }
}

fn replay_at(at_secs: u64, nonce_model: Option<String>) -> Replay {
    let m = meta();
    Replay {
        model_collateral: collateral("collateral-model.json", at_secs),
        gateway_collateral: collateral("collateral-gateway.json", at_secs),
        gpu: Arc::new(RecordedGpu),
        nonce_model: nonce_model.unwrap_or_else(|| m["nonce_model"].as_str().unwrap().into()),
        nonce_gateway: m["nonce_gateway"].as_str().unwrap().into(),
    }
}

async fn run(
    policy: Policy,
    replay: Replay,
    model_report: String,
    gateway_report: String,
) -> Result<ModelIdentity, String> {
    let url = serve(model_report, gateway_report).await;
    let http = reqwest::Client::new();
    NearAiAttestor::replaying(policy, replay)
        .verify_model(&ReportRequest {
            gateway_url: &url,
            model: MODEL,
            nonce: "unused",
            http: &http,
            socks_url: None,
        })
        .await
}

fn texts() -> (String, String) {
    (
        String::from_utf8(fixture("model-report.json")).unwrap(),
        String::from_utf8(fixture("gateway-report.json")).unwrap(),
    )
}

fn recorded_at() -> u64 {
    meta()["recorded_at_secs"].as_u64().unwrap()
}

#[tokio::test]
async fn the_recorded_evidence_verifies_and_binds_the_key() {
    let (m, g) = texts();
    let id = run(
        policy(deployments()),
        replay_at(recorded_at(), None),
        m.clone(),
        g,
    )
    .await
    .unwrap();
    let report: Value = serde_json::from_str(&m).unwrap();
    let key = report["model_attestations"][0]["signing_public_key"]
        .as_str()
        .unwrap();
    assert_eq!(hex::encode(id.model_pub), key.trim_start_matches("0x"));
    assert_eq!(
        id.model_signing_address.trim_start_matches("0x"),
        key.trim_start_matches("0x")
    );
    let verified = id.verified_model.unwrap();
    assert_eq!(verified.evidence.tcb_status, TcbStatus::UpToDate);
    assert_eq!(verified.gpu_evidence, GpuEvidenceStatus::Verified);
    assert_eq!(
        id.verified_gateway.unwrap().evidence.tcb_status,
        TcbStatus::UpToDate
    );
}

#[tokio::test]
async fn a_deployment_the_policy_does_not_list_is_refused() {
    let (m, g) = texts();
    // The right entry for the gateway, nothing for the model.
    let only_gateway: Vec<_> = deployments()
        .into_iter()
        .filter(|d| d.role == "gateway")
        .collect();
    let e = run(
        policy(only_gateway),
        replay_at(recorded_at(), None),
        m.clone(),
        g.clone(),
    )
    .await
    .unwrap_err();
    assert!(
        e.contains("measurement_not_accepted") || e.contains("quote.verification_failed"),
        "{e}"
    );

    // A changed rtmr1 is a different machine.
    let mut changed = deployments();
    changed
        .iter_mut()
        .find(|d| d.role == "model")
        .unwrap()
        .rtmr1 = "00".repeat(48);
    assert!(run(policy(changed), replay_at(recorded_at(), None), m, g)
        .await
        .is_err());
}

#[tokio::test]
async fn compose_and_image_hashes_must_come_from_the_same_entry_as_the_measurements() {
    let (m, g) = texts();
    let model = deployments()
        .into_iter()
        .find(|d| d.role == "model")
        .unwrap();
    let gateway = deployments()
        .into_iter()
        .find(|d| d.role == "gateway")
        .unwrap();
    // Entry A has the right Intel measurements and a wrong compose hash; entry B has the right
    // compose hash and wrong measurements. Taken apart each half matches, together they must not.
    let a = Deployment {
        compose_hashes: vec!["11".repeat(32)],
        ..model.clone()
    };
    let b = Deployment {
        mr_td: "22".repeat(48),
        ..model
    };
    let e = run(
        policy(vec![a, b, gateway]),
        replay_at(recorded_at(), None),
        m,
        g,
    )
    .await
    .unwrap_err();
    assert!(e.contains("provenance"), "{e}");
}

#[tokio::test]
async fn a_report_for_another_nonce_is_refused() {
    let (m, g) = texts();
    let e = run(
        policy(deployments()),
        replay_at(recorded_at(), Some("ab".repeat(32))),
        m,
        g,
    )
    .await
    .unwrap_err();
    assert!(e.contains("nonce"), "{e}");
}

#[tokio::test]
async fn collateral_that_has_expired_is_refused() {
    let (m, g) = texts();
    let a_year_later = recorded_at() + 366 * 86_400;
    let e = run(policy(deployments()), replay_at(a_year_later, None), m, g)
        .await
        .unwrap_err();
    assert!(e.contains("quote"), "{e}");
}

#[tokio::test]
async fn a_changed_quote_a_changed_key_and_missing_gpu_evidence_are_refused() {
    let (m, g) = texts();
    let replay = || replay_at(recorded_at(), None);
    let flip = |text: &str, field: &str| -> String {
        let mut v: Value = serde_json::from_str(text).unwrap();
        let entry = if v.get("model_attestations").is_some() {
            &mut v["model_attestations"][0]
        } else {
            &mut v["gateway_attestation"]
        };
        let s = entry[field].as_str().unwrap().to_string();
        let mut bytes = s.into_bytes();
        // Inside the signed TD report (the first 632 bytes of the quote) for the quote; the
        // middle of a key.
        let at = if field == "intel_quote" {
            700
        } else {
            bytes.len() / 2
        };
        bytes[at] = if bytes[at] == b'0' { b'1' } else { b'0' };
        entry[field] = Value::String(String::from_utf8(bytes).unwrap());
        v.to_string()
    };
    // A bit of the Intel quote.
    assert!(run(
        policy(deployments()),
        replay(),
        flip(&m, "intel_quote"),
        g.clone()
    )
    .await
    .is_err());
    // A bit of the encryption key: it no longer equals the attested signer.
    let e = run(
        policy(deployments()),
        replay(),
        flip(&m, "signing_public_key"),
        g.clone(),
    )
    .await
    .unwrap_err();
    assert!(e.contains("key") || e.contains("binding"), "{e}");
    // No GPU evidence: `Required` refuses it.
    let mut v: Value = serde_json::from_str(&m).unwrap();
    v["model_attestations"][0]["nvidia_payload"] = Value::Null;
    let e = run(policy(deployments()), replay(), v.to_string(), g)
        .await
        .unwrap_err();
    assert!(e.contains("gpu"), "{e}");
}

#[test]
fn the_draft_policy_loads_once_signed_and_refuses_a_lenient_one() {
    let signed = policy(deployments());
    let json = serde_json::to_vec(&signed).unwrap();
    let pk = signer().verifying_key().to_bytes();
    assert_eq!(Policy::load(&json, &pk, 1).unwrap().deployments.len(), 2);
    let mut lenient = signed;
    lenient.accepted_tcb_statuses.push("OutOfDate".into());
    let lenient = lenient.sign(&signer());
    assert!(matches!(
        Policy::load(&serde_json::to_vec(&lenient).unwrap(), &pk, 1),
        Err(PolicyError::Lenient(_))
    ));
}

// ---- completion signatures ----

fn verified(sk: &SigningKey, gateway: bool) -> (ModelIdentity, SigningIdentity) {
    let addr = hex::encode(sk.verifying_key().to_bytes());
    let signer = SigningIdentity {
        signing_algo: SigningAlgo::Ed25519,
        signing_address: addr.clone(),
    };
    let evidence = VerifiedAttestationEvidence {
        signer: signer.clone(),
        tcb_status: TcbStatus::UpToDate,
        advisory_ids: vec![],
        deployment: MeasuredDeployment {
            app_compose: String::new(),
            runtime_measurements: RuntimeMeasurements::default(),
        },
        deployment_provenance: DeploymentProvenanceStatus::NotChecked,
    };
    let id = ModelIdentity {
        model_pub: sk.verifying_key().to_bytes(),
        model_signing_address: addr.clone(),
        gateway_signing_address: addr,
        verified_model: (!gateway).then(|| VerifiedModelAttestation {
            signing_public_key: None,
            evidence: evidence.clone(),
            gpu_evidence: GpuEvidenceStatus::Verified,
        }),
        verified_gateway: gateway.then(|| VerifiedGatewayAttestation {
            evidence,
            tls_binding: GatewayTlsBinding::None,
        }),
    };
    (id, signer)
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn wire(kind: &str, text: &str, sk: &SigningKey) -> Value {
    json!({
        "signature_kind": kind, "text": text,
        "signature": hex::encode(sk.sign(text.as_bytes()).to_bytes()),
        "signing_address": hex::encode(sk.verifying_key().to_bytes()),
        "signing_algo": "ed25519",
    })
}

#[test]
fn completion_signatures_map_to_model_signed_gateway_signed_or_not_verified() {
    let attestor = NearAiAttestor::new(policy(deployments()));
    let sk = SigningKey::from_bytes(&[3; 32]);
    let (req, resp) = (
        br#"{"model":"m","messages":[]}"#.to_vec(),
        b"the answer".to_vec(),
    );

    let (model_id, _) = verified(&sk, false);
    let text = format!("m:{}:{}", sha(&req), sha(&resp));
    let sig = wire("provider_tee", &text, &sk);
    assert_eq!(
        attestor.verify_signature(&model_id, &sig, "m", &req, &resp),
        Trust::ModelSigned
    );
    // Other bytes than the ones signed.
    assert!(matches!(
        attestor.verify_signature(&model_id, &sig, "m", &req, b"another answer"),
        Trust::NotVerified(_)
    ));
    // A signature by a key the attestation did not establish.
    let other = SigningKey::from_bytes(&[4; 32]);
    assert!(matches!(
        attestor.verify_signature(
            &model_id,
            &wire("provider_tee", &text, &other),
            "m",
            &req,
            &resp
        ),
        Trust::NotVerified(_)
    ));
    // The gateway's signature does not count as the model's.
    let (gw_id, _) = verified(&sk, true);
    let gtext = format!("{}:{}", sha(&req), sha(&resp));
    assert_eq!(
        attestor.verify_signature(&gw_id, &wire("gateway", &gtext, &sk), "m", &req, &resp),
        Trust::GatewaySigned
    );
    assert!(matches!(
        attestor.verify_signature(&gw_id, &wire("provider_tee", &text, &sk), "m", &req, &resp),
        Trust::NotVerified(_)
    ));
    // Malformed.
    assert!(matches!(
        attestor.verify_signature(&model_id, &json!({"text": 1}), "m", &req, &resp),
        Trust::NotVerified(_)
    ));
    let _ = CompletionSignatureKind::Gateway;
}
