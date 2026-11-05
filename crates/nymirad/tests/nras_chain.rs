//! NVIDIA's GPU verdict checked down to the certificate chain of its signing key. The chains are
//! generated here (a root, an intermediate and a leaf, all P-384), so no case needs NVIDIA.
//!
//! Two seams: `NrasChain` as a `GpuEvidenceVerifier` against a stand-in NRAS host, for every
//! refusal; and `NearAiAttestor::verify_model` against the recorded Intel evidence and the same
//! host, for the pass and for the policy that ships today.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use nearai_inference_sdk::{GpuEvidenceVerifier, VerificationError};
use nymira_operator::clock::ManualClock;
use nymirad::attest::{Attestor, Deployment, NvidiaPin, Policy, ReportRequest};
use nymirad::nearai::{Collateral, NearAiAttestor, Replay};
use nymirad::nras::NrasChain;
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 2026-11-05 00:00 UTC.
const NOW: u64 = 1_793_836_800;
const NONCE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const ISSUER: &str = "https://nras.attestation.nvidia.com";

type Date = (i32, u8, u8);
const LONG: Date = (2030, 1, 1);
const PAST: Date = (2025, 1, 1);

struct Pki {
    root: Vec<u8>,
    inter: Vec<u8>,
    leaf: Vec<u8>,
    leaf_key: KeyPair,
}

fn params(name: &str, ca: bool, not_before: Date, not_after: Date) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::new()).unwrap();
    p.distinguished_name.push(DnType::CommonName, name);
    if ca {
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    }
    p.not_before = date_time_ymd(not_before.0, not_before.1, not_before.2);
    p.not_after = date_time_ymd(not_after.0, not_after.1, not_after.2);
    p
}

impl Pki {
    fn new(leaf_end: Date, inter_end: Date) -> Self {
        Self::with_dates((2020, 1, 1), leaf_end, inter_end)
    }

    fn with_dates(leaf_start: Date, leaf_end: Date, inter_end: Date) -> Self {
        let alg = &PKCS_ECDSA_P384_SHA384;
        let root_key = KeyPair::generate_for(alg).unwrap();
        let root_params = params("Test Root", true, (2020, 1, 1), LONG);
        let root = root_params.self_signed(&root_key).unwrap();
        let root_issuer = Issuer::new(root_params, root_key);

        let inter_key = KeyPair::generate_for(alg).unwrap();
        let inter_params = params("Test Intermediate", true, (2020, 1, 1), inter_end);
        let inter = inter_params.signed_by(&inter_key, &root_issuer).unwrap();
        let inter_issuer = Issuer::new(inter_params, inter_key);

        let leaf_key = KeyPair::generate_for(alg).unwrap();
        let leaf = params("Test Leaf", false, leaf_start, leaf_end)
            .signed_by(&leaf_key, &inter_issuer)
            .unwrap();
        Self {
            root: root.der().to_vec(),
            inter: inter.der().to_vec(),
            leaf: leaf.der().to_vec(),
            leaf_key,
        }
    }

    fn chain(&self) -> Vec<Vec<u8>> {
        vec![self.leaf.clone(), self.inter.clone(), self.root.clone()]
    }
}

fn pin(name: &str, der: &[u8]) -> NvidiaPin {
    NvidiaPin {
        name: name.into(),
        sha256: hex::encode(Sha256::digest(der)),
        official_source_verified: false,
    }
}

fn point(key: &KeyPair) -> (String, String) {
    let raw = key.public_key_raw();
    (
        URL_SAFE_NO_PAD.encode(&raw[1..49]),
        URL_SAFE_NO_PAD.encode(&raw[49..]),
    )
}

/// How a stand-in NRAS host misbehaves. The default is a correct answer.
struct Knobs {
    /// The `x5c` list the JWKS key carries; `None` for a key without one.
    x5c: Option<Vec<Vec<u8>>>,
    /// The key the token is signed with, when it is not the leaf's.
    signer: Option<KeyPair>,
    /// The public key the JWKS lists, when it is not the leaf's.
    jwk_key: Option<KeyPair>,
    alg: Algorithm,
    nonce: Option<String>,
    overall: bool,
    exp: u64,
    /// The `iat`, `nbf` and `exp` base, so the host can follow the real clock.
    now: u64,
}

impl Knobs {
    fn new(pki: &Pki) -> Self {
        Self {
            x5c: Some(pki.chain()),
            signer: None,
            jwk_key: None,
            alg: Algorithm::ES384,
            nonce: None,
            overall: true,
            exp: 3600,
            now: NOW,
        }
    }
}

/// The answer and the key set for a payload nonce.
fn answer(pki: &Pki, k: &Knobs, nonce: &str) -> (Value, Value) {
    let signer = k.signer.as_ref().unwrap_or(&pki.leaf_key);
    let claims = json!({
        "iss": ISSUER, "iat": k.now - 60, "nbf": k.now - 60, "exp": k.now + k.exp,
        "eat_nonce": k.nonce.clone().unwrap_or_else(|| nonce.to_owned()),
        "x-nvidia-overall-att-result": k.overall,
    });
    let mut header = Header::new(k.alg);
    header.kid = Some("test-nras".into());
    let jwt = encode(
        &header,
        &claims,
        &EncodingKey::from_ec_der(signer.serialized_der()),
    )
    .unwrap();
    let (x, y) = point(k.jwk_key.as_ref().unwrap_or(&pki.leaf_key));
    let mut jwk = json!({"kid": "test-nras", "kty": "EC", "crv": "P-384", "x": x, "y": y});
    if let Some(chain) = &k.x5c {
        jwk["x5c"] = chain.iter().map(|c| STANDARD.encode(c)).collect();
    }
    (
        json!([["JWT", jwt], {"later": "ignored"}]),
        json!({"keys": [jwk]}),
    )
}

type Make = Box<dyn Fn(&str) -> (Value, Value) + Send + Sync>;

/// A stand-in NRAS host: the evidence POST and the JWKS GET.
async fn nras_host(make: Make) -> String {
    async fn evidence(State(make): State<Arc<Make>>, body: String) -> Json<Value> {
        let nonce = serde_json::from_str::<Value>(&body).unwrap()["nonce"]
            .as_str()
            .unwrap()
            .to_owned();
        Json(make(&nonce).0)
    }
    async fn jwks(State(make): State<Arc<Make>>) -> Json<Value> {
        Json(make(NONCE).1)
    }
    let app = Router::new()
        .route("/v3/attest/gpu", post(evidence))
        .route("/jwks", get(jwks))
        .with_state(Arc::new(make));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn check(
    pki: Pki,
    knobs: impl FnOnce(&mut Knobs),
    pins: &[NvidiaPin],
    now: u64,
) -> Result<(), VerificationError> {
    let mut k = Knobs::new(&pki);
    knobs(&mut k);
    let url = nras_host(Box::new(move |n| answer(&pki, &k, n))).await;
    NrasChain::new(
        reqwest::Client::new(),
        format!("{url}/v3/attest/gpu"),
        format!("{url}/jwks"),
        pins,
        Arc::new(ManualClock::new(now * 1000)),
    )
    .verify(&json!({"nonce": NONCE, "evidence_list": []}).to_string())
    .await
}

fn refused(result: Result<(), VerificationError>, reason: &str) {
    let e = result.expect_err("the verdict must be refused");
    assert!(e.to_string().ends_with(reason), "{reason}: {e}");
}

#[tokio::test]
async fn a_token_signed_by_a_key_that_chains_to_a_pinned_certificate_passes() {
    // The pinned certificate may be the intermediate or the root.
    for pinned in ["intermediate", "root"] {
        let pki = Pki::new(LONG, LONG);
        let pins = [pin(
            pinned,
            if pinned == "root" {
                &pki.root
            } else {
                &pki.inter
            },
        )];
        check(pki, |_| {}, &pins, NOW).await.unwrap();
    }
}

#[tokio::test]
async fn a_chain_that_ends_in_no_pinned_certificate_is_refused() {
    let pki = Pki::new(LONG, LONG);
    let other = Pki::new(LONG, LONG);
    let pins = [pin("other root", &other.root)];
    refused(check(pki, |_| {}, &pins, NOW).await, "chain_untrusted");
    // No pins at all pins nothing.
    refused(
        check(Pki::new(LONG, LONG), |_| {}, &[], NOW).await,
        "chain_untrusted",
    );
}

#[tokio::test]
async fn an_expired_leaf_or_intermediate_is_refused() {
    let pki = Pki::new(PAST, LONG);
    let pins = [pin("root", &pki.root)];
    refused(check(pki, |_| {}, &pins, NOW).await, "leaf_expired");

    let pki = Pki::new(LONG, PAST);
    let pins = [pin("intermediate", &pki.inter)];
    refused(check(pki, |_| {}, &pins, NOW).await, "intermediate_expired");
}

#[tokio::test]
async fn validity_is_checked_at_the_time_the_clock_gives() {
    // A leaf valid until 1 Dec 2026 passes on 5 Nov and fails a month later; one that starts on
    // 10 Nov is not yet valid on the 5th.
    let end = (2026, 12, 1);
    let pki = Pki::new(end, LONG);
    let pins = [pin("intermediate", &pki.inter)];
    let after = NOW + 40 * 86_400;
    refused(
        check(pki, |k| k.now = after, &pins, after).await,
        "leaf_expired",
    );

    let pki = Pki::new(end, LONG);
    let pins = [pin("intermediate", &pki.inter)];
    check(pki, |_| {}, &pins, NOW).await.unwrap();

    let pki = Pki::with_dates((2026, 11, 10), LONG, LONG);
    let pins = [pin("intermediate", &pki.inter)];
    refused(
        check(pki, |_| {}, &pins, NOW).await,
        "certificate_not_yet_valid",
    );
}

#[tokio::test]
async fn a_key_without_x5c_is_refused() {
    let pki = Pki::new(LONG, LONG);
    let pins = [pin("intermediate", &pki.inter)];
    refused(
        check(pki, |k| k.x5c = None, &pins, NOW).await,
        "x5c_missing",
    );
}

#[tokio::test]
async fn an_es256_token_is_refused() {
    let pki = Pki::new(LONG, LONG);
    let pins = [pin("intermediate", &pki.inter)];
    let es256 = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    refused(
        check(
            pki,
            |k| {
                k.alg = Algorithm::ES256;
                k.signer = Some(es256);
            },
            &pins,
            NOW,
        )
        .await,
        "unsupported_algorithm",
    );
}

#[tokio::test]
async fn a_token_signed_by_a_key_outside_the_chain_is_refused() {
    let pins = |pki: &Pki| [pin("intermediate", &pki.inter)];
    let attacker = || KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();

    // The relay lists its own key next to a genuine chain.
    let pki = Pki::new(LONG, LONG);
    let p = pins(&pki);
    refused(
        check(
            pki,
            |k| {
                let key = attacker();
                k.jwk_key = Some(attacker());
                k.signer = Some(key);
            },
            &p,
            NOW,
        )
        .await,
        "key_not_in_chain",
    );

    // The relay lists the genuine key and signs with its own.
    let pki = Pki::new(LONG, LONG);
    let p = pins(&pki);
    refused(
        check(pki, |k| k.signer = Some(attacker()), &p, NOW).await,
        "invalid_signature",
    );
}

#[tokio::test]
async fn a_token_for_another_nonce_a_false_verdict_and_an_expired_token_are_refused() {
    let pki = Pki::new(LONG, LONG);
    let p = [pin("intermediate", &pki.inter)];
    refused(
        check(pki, |k| k.nonce = Some("22".repeat(32)), &p, NOW).await,
        "nonce_mismatch",
    );

    let pki = Pki::new(LONG, LONG);
    let p = [pin("intermediate", &pki.inter)];
    let e = check(pki, |k| k.overall = false, &p, NOW)
        .await
        .unwrap_err();
    assert!(matches!(
        e,
        VerificationError::GpuAttestationRejected { origin: "nras" }
    ));

    let pki = Pki::new(LONG, LONG);
    let p = [pin("intermediate", &pki.inter)];
    // exp = now - 60 + 3600 - 3600: the token expired a minute ago.
    refused(check(pki, |k| k.exp = 0, &p, NOW).await, "expired");
}

// ---- through verify_model ----

const MODEL: &str = "z-ai/glm-5.3-flash";

fn fixture(name: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/near-ai-recorded");
    std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn meta() -> Value {
    serde_json::from_slice(&fixture("meta.json")).unwrap()
}

fn deployments() -> Vec<Deployment> {
    let draft: Policy = serde_json::from_slice(&fixture("policy-draft.json")).unwrap();
    draft.deployments
}

fn collateral(name: &str, at_secs: u64) -> Collateral {
    Collateral::Recorded {
        collateral: Box::new(serde_json::from_slice(&fixture(name)).unwrap()),
        at_secs,
    }
}

fn replay() -> Replay {
    let (m, at) = (meta(), meta()["recorded_at_secs"].as_u64().unwrap());
    Replay {
        model_collateral: collateral("collateral-model.json", at),
        gateway_collateral: collateral("collateral-gateway.json", at),
        gpu: None,
        nonce_model: m["nonce_model"].as_str().unwrap().into(),
        nonce_gateway: m["nonce_gateway"].as_str().unwrap().into(),
    }
}

/// The recorded reports behind a local server.
async fn gateway() -> String {
    use axum::extract::Query;
    async fn report(
        State(b): State<Arc<(String, String)>>,
        Query(q): Query<std::collections::HashMap<String, String>>,
    ) -> String {
        if q.contains_key("model") {
            b.0.clone()
        } else {
            b.1.clone()
        }
    }
    let bodies = (
        String::from_utf8(fixture("model-report.json")).unwrap(),
        String::from_utf8(fixture("gateway-report.json")).unwrap(),
    );
    let app = Router::new()
        .route("/v1/attestation/report", get(report))
        .with_state(Arc::new(bodies));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn policy(pins: Vec<NvidiaPin>) -> Policy {
    Policy {
        version: 1,
        attestation_root_pk: String::new(),
        accepted_measurements: vec![],
        deployments: deployments(),
        accepted_tcb_statuses: vec!["UpToDate".into()],
        nvidia_pins: pins,
        signature: String::new(),
    }
}

async fn verify_model(attestor: NearAiAttestor) -> Result<(), String> {
    let url = gateway().await;
    let http = reqwest::Client::new();
    attestor
        .verify_model(&ReportRequest {
            gateway_url: &url,
            model: MODEL,
            nonce: "unused",
            http: &http,
            socks_url: None,
        })
        .await
        .map(|_| ())
}

/// The clock the recorded Intel collateral is checked at; the NRAS chain follows it.
fn recorded_at() -> u64 {
    meta()["recorded_at_secs"].as_u64().unwrap()
}

#[tokio::test]
async fn verify_model_passes_with_a_pinned_chain_and_refuses_an_unpinned_one() {
    let at = recorded_at();
    let pki = Pki::new(LONG, LONG);
    let pins = vec![pin("intermediate", &pki.inter)];
    let mut k = Knobs::new(&pki);
    k.now = at;
    let url = nras_host(Box::new(move |n| answer(&pki, &k, n))).await;
    let attestor = |pins: Vec<NvidiaPin>| {
        NearAiAttestor::replaying(policy(pins), replay())
            .with_nras(format!("{url}/v3/attest/gpu"), format!("{url}/jwks"))
            .with_clock(Arc::new(ManualClock::new(at * 1000)))
    };
    verify_model(attestor(pins)).await.unwrap();

    let other = Pki::new(LONG, LONG);
    let e = verify_model(attestor(vec![pin("other", &other.root)]))
        .await
        .unwrap_err();
    assert!(e.contains("chain_untrusted"), "{e}");
}

/// The policy that ships carries no NVIDIA pins. It must keep loading, and a chat must keep
/// verifying against the key NVIDIA's service serves, so a merge does not break production.
#[tokio::test]
async fn the_shipped_policy_still_loads_and_the_gpu_check_still_passes_with_it() {
    let signer: [u8; 32] = hex::decode(include_str!("../pins/policy-signer.pub").trim())
        .unwrap()
        .try_into()
        .unwrap();
    let shipped = Policy::load(
        include_bytes!("../pins/attestation-policy.json"),
        &signer,
        2,
    )
    .expect("signed by the pinned key");
    assert!(shipped.nvidia_pins.is_empty());

    // The SDK checks the token against the real clock, so the host follows it.
    let real_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let pki = Pki::new(LONG, LONG);
    let mut k = Knobs::new(&pki);
    k.x5c = None;
    k.now = real_now;
    let url = nras_host(Box::new(move |n| answer(&pki, &k, n))).await;
    let at = recorded_at();
    let attestor = NearAiAttestor::replaying(shipped, replay())
        .with_nras(format!("{url}/v3/attest/gpu"), format!("{url}/jwks"));
    // The Intel collateral is checked at its recorded time inside the replay; only the GPU step
    // runs live here.
    let _ = at;
    verify_model(attestor).await.unwrap();
}

#[test]
fn status_says_whether_the_gpu_chain_is_pinned() {
    let none = nymirad::nearai::gpu_chain_line(&policy(vec![]));
    assert!(none.starts_with("GPU chain: not pinned"), "{none}");
    let pki = Pki::new(LONG, LONG);
    let pinned = nymirad::nearai::gpu_chain_line(&policy(vec![
        pin("a", &pki.inter),
        pin("b", &pki.root),
    ]));
    assert!(
        pinned.starts_with("GPU chain: pinned (2 certificates, 2 not checked"),
        "{pinned}"
    );
}
