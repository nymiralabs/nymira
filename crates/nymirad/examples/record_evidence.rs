//! Records NEAR AI's attestation evidence for offline tests and drafts a policy from it.
//!
//! It needs no API key: the report endpoints are public. It does talk to NEAR AI, Intel's PCS and
//! NVIDIA's NRAS directly from this machine, which a user's `nymirad` never does, so run it from a
//! development machine.
//!
//!   cargo run -p nymirad --example record_evidence -- [OUT_DIR] [MODEL]
//!
//! Writes the two raw reports (with the nonces the fixtures were fetched under), each quote's
//! Intel collateral and the time of recording, and `policy-draft.json` with the deployments seen
//! (unsigned: a person checks them against NEAR AI's published builds, then signs offline).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nearai_inference_sdk::{
    verify_gateway_attestation, verify_model_attestation, AttestationClient, AttestationVerifiers,
    GatewayAttestationFetchOptions, ModelAttestationVerifiers, NrasGpuEvidenceVerifier,
    SigningAlgo, DEFAULT_INTEL_PCCS_URL,
};
use nymirad::attest::{Deployment, Policy};
use nymirad::nearai::{Collateral, IntelTdx, Observed};
use serde_json::{json, Value};

const GATEWAY: &str = "https://cloud-api.near.ai";

fn nonce() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

async fn get(http: &reqwest::Client, url: &str, query: &[(&str, &str)]) -> String {
    http.get(url)
        .query(query)
        .header("x-no-aliasing", "true")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .unwrap_or_else(|e| panic!("GET {url}: {}", e.without_url()))
        .text()
        .await
        .expect("body")
}

/// Adds what was seen to `entries`: a new compose hash joins the entry of the same machine image.
fn merge(entries: &mut Vec<Deployment>, role: &str, o: &Observed) {
    let seen = deployment(role, o);
    match entries.iter_mut().find(|d| {
        (
            &d.role,
            &d.os_image_hash,
            &d.mr_td,
            &d.rtmr0,
            &d.rtmr1,
            &d.rtmr2,
        ) == (
            &seen.role,
            &seen.os_image_hash,
            &seen.mr_td,
            &seen.rtmr0,
            &seen.rtmr1,
            &seen.rtmr2,
        )
    }) {
        Some(d) => {
            for h in seen.compose_hashes {
                if !d.compose_hashes.contains(&h) {
                    d.compose_hashes.push(h);
                }
            }
        }
        None => entries.push(seen),
    }
}

fn deployment(role: &str, o: &Observed) -> Deployment {
    Deployment {
        role: role.into(),
        compose_hashes: vec![o.compose_hash.clone().expect("compose hash")],
        os_image_hash: o.os_image_hash.clone().expect("os image hash"),
        mr_td: o.mr_td.clone(),
        rtmr0: o.rtmr0.clone(),
        rtmr1: o.rtmr1.clone(),
        rtmr2: o.rtmr2.clone(),
    }
}

/// Verifies the model and gateway reports `api` serves with observing verifiers (nothing is
/// accepted by policy) and returns what each measured. `nonces` fixes the nonces for a replay.
async fn observe(
    api: &AttestationClient,
    model: &str,
    nonces: Option<(&str, &str)>,
    model_collateral: Collateral,
    gateway_collateral: Collateral,
) -> Vec<(&'static str, Observed)> {
    let mut found = Vec::new();
    let sink = Arc::new(Mutex::new(Vec::new()));
    let tdx = IntelTdx::observing(model_collateral, sink.clone());
    let dep = tdx.deployment_check();
    let nras = NrasGpuEvidenceVerifier::default();
    let fetched = match nonces {
        Some((nm, _)) => {
            api.fetch_model_attestations_with_nonce(
                model,
                Some(SigningAlgo::Ed25519),
                None,
                nm.into(),
            )
            .await
        }
        None => {
            api.fetch_model_attestations(model, Some(SigningAlgo::Ed25519), None)
                .await
        }
    }
    .expect("model report");
    for a in fetched
        .attestations
        .iter()
        .filter(|a| a.signing_public_key.is_some())
    {
        // A recorded report cannot be checked by NVIDIA again (its token expires in minutes), so
        // GPU evidence is only checked on live fetches.
        let gpu: Option<&NrasGpuEvidenceVerifier> = nonces.is_none().then_some(&nras);
        let verifiers = ModelAttestationVerifiers {
            tdx_quote: Some(&tdx),
            deployment: Some(&dep),
            gpu_evidence: gpu.map(|g| g as &dyn nearai_inference_sdk::GpuEvidenceVerifier),
        };
        let policy = nearai_inference_sdk::ModelAttestationPolicy::default();
        match verify_model_attestation(a, &fetched.client_binding, Some(&policy), verifiers).await {
            Ok(v) => {
                println!(
                    "model: tcb {:?}, gpu {:?}, advisories {:?}",
                    v.evidence.tcb_status, v.gpu_evidence, v.evidence.advisory_ids
                );
                if let Some(o) = sink.lock().unwrap().last().cloned() {
                    found.push(("model", o));
                }
                break;
            }
            Err(e) => println!("model attestation rejected: {e} ({})", e.code()),
        }
    }
    let sink_g = Arc::new(Mutex::new(Vec::new()));
    let tdx_g = IntelTdx::observing(gateway_collateral, sink_g.clone());
    let dep_g = tdx_g.deployment_check();
    let options = GatewayAttestationFetchOptions {
        signing_algo: Some(SigningAlgo::Ed25519),
        include_spki_fingerprint: false,
    };
    let g = match nonces {
        Some((_, ng)) => {
            api.fetch_gateway_attestation_with_nonce(options, ng.into())
                .await
        }
        None => api.fetch_gateway_attestation(options).await,
    }
    .expect("gateway report");
    match verify_gateway_attestation(
        &g.attestation,
        &g.client_binding,
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_g),
            deployment: Some(&dep_g),
        },
    )
    .await
    {
        Ok(v) => {
            println!(
                "gateway: tcb {:?}, advisories {:?}",
                v.evidence.tcb_status, v.evidence.advisory_ids
            );
            if let Some(o) = sink_g.lock().unwrap().last().cloned() {
                found.push(("gateway", o));
            }
        }
        Err(e) => println!("gateway attestation rejected: {e} ({})", e.code()),
    }
    found
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "crates/nymirad/tests/fixtures/near-ai-recorded".into()),
    );
    let model = args.next().unwrap_or_else(|| "z-ai/glm-5.3-flash".into());
    std::fs::create_dir_all(&out).unwrap();
    let http = reqwest::Client::new();
    let (nonce_model, nonce_gateway) = (nonce(), nonce());

    // 1. The raw reports, exactly as served.
    let url = format!("{GATEWAY}/v1/attestation/report");
    let model_report = get(
        &http,
        &url,
        &[
            ("model", &model),
            ("provider", "near"),
            ("nonce", &nonce_model),
            ("include_tls_fingerprint", "false"),
            ("signing_algo", "ed25519"),
        ],
    )
    .await;
    let gateway_report = get(
        &http,
        &url,
        &[
            ("nonce", &nonce_gateway),
            ("signing_algo", "ed25519"),
            ("include_tls_fingerprint", "false"),
        ],
    )
    .await;
    std::fs::write(out.join("model-report.json"), &model_report).unwrap();
    std::fs::write(out.join("gateway-report.json"), &gateway_report).unwrap();

    // 2. Intel collateral for each quote, and the time it is valid at.
    let recorded_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (name, body, path) in [
        ("model", &model_report, "/model_attestations/0/intel_quote"),
        (
            "gateway",
            &gateway_report,
            "/gateway_attestation/intel_quote",
        ),
    ] {
        let value: Value = serde_json::from_str(body).unwrap();
        let quote = hex::decode(
            value
                .pointer(path)
                .and_then(Value::as_str)
                .expect("intel_quote"),
        )
        .expect("quote hex");
        let collateral = dcap_qvl::collateral::get_collateral(DEFAULT_INTEL_PCCS_URL, &quote)
            .await
            .unwrap_or_else(|e| panic!("collateral for the {name} quote: {e}"));
        std::fs::write(
            out.join(format!("collateral-{name}.json")),
            serde_json::to_vec_pretty(&collateral).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(
        out.join("meta.json"),
        serde_json::to_vec_pretty(&json!({
            "model": model, "gateway": GATEWAY, "recorded_at_secs": recorded_at,
            "nonce_model": nonce_model, "nonce_gateway": nonce_gateway,
            "pccs": DEFAULT_INTEL_PCCS_URL,
        }))
        .unwrap(),
    )
    .unwrap();

    // 3. What the recorded reports measured, read through the same verifiers the tests use (so the
    //    draft always contains the recording's own compose hash), and then a few live fetches: one
    //    machine image runs on several VMs whose compose hashes differ.
    let mut seen: Vec<(&str, Observed)> = Vec::new();
    {
        use axum::{extract::Query, routing::get, Router};
        let (m, g) = (model_report.clone(), gateway_report.clone());
        let app = Router::new().route(
            "/v1/attestation/report",
            get(
                move |Query(q): Query<std::collections::HashMap<String, String>>| {
                    let body = if q.contains_key("model") {
                        m.clone()
                    } else {
                        g.clone()
                    };
                    async move { body }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let api = AttestationClient::with_base_url(String::new(), &local).unwrap();
        let recorded = |name: &str| {
            let c: dcap_qvl::QuoteCollateralV3 = serde_json::from_slice(
                &std::fs::read(out.join(format!("collateral-{name}.json"))).unwrap(),
            )
            .unwrap();
            Collateral::Recorded {
                collateral: Box::new(c),
                at_secs: recorded_at,
            }
        };
        seen.extend(
            observe(
                &api,
                &model,
                Some((&nonce_model, &nonce_gateway)),
                recorded("model"),
                recorded("gateway"),
            )
            .await,
        );
    }
    let api = AttestationClient::with_base_url(String::new(), &format!("{GATEWAY}/v1")).unwrap();
    let live = || Collateral::Live {
        client: None,
        pccs_url: DEFAULT_INTEL_PCCS_URL.into(),
    };
    for _ in 0..3 {
        seen.extend(observe(&api, &model, None, live(), live()).await);
    }
    let (model_seen, gateway_seen): (Vec<_>, Vec<_>) =
        seen.into_iter().partition(|(role, _)| *role == "model");

    // 4. The draft policy.
    // An earlier draft in the same directory is extended, so repeated runs collect the compose
    // hashes of the different VMs behind one model.
    let mut deployments: Vec<Deployment> = std::fs::read(out.join("policy-draft.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Policy>(&b).ok())
        .map(|p| p.deployments)
        .unwrap_or_default();
    for (role, o) in model_seen.iter().chain(&gateway_seen) {
        merge(&mut deployments, role, o);
    }
    let draft = Policy {
        version: 1,
        attestation_root_pk: String::new(),
        accepted_measurements: vec![],
        deployments,
        accepted_tcb_statuses: vec!["UpToDate".into()],
        signature: String::new(),
    };
    std::fs::write(
        out.join("policy-draft.json"),
        serde_json::to_vec_pretty(&draft).unwrap(),
    )
    .unwrap();
    println!(
        "wrote {} ({} deployments)",
        out.display(),
        draft.deployments.len()
    );
}
