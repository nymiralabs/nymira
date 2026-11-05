//! NRAS verification through the real attestor, recorded Intel evidence, and loopback services.

use axum::{
    routing::{get, post},
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use nymirad::attest::{Attestor, ModelIdentity, NvidiaCertificate, Policy, ReportRequest};
use nymirad::nearai::{Collateral, NearAiAttestor, Replay};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P384_SHA384,
};
use serde_json::{json, Value};
use time::OffsetDateTime;
use zkapi_proof::compact::CompactSigner;
use zkapi_types::Felt252;

const AT: u64 = 1_793_643_311;
const MODEL: &str = "z-ai/glm-5.3-flash";
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/near-ai-recorded")
            .join(name),
    )
    .unwrap()
}
fn params(name: &str, ca: bool, end: u64) -> CertificateParams {
    let mut p = CertificateParams::default();
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name.push(DnType::CommonName, name);
    p.not_before = OffsetDateTime::from_unix_timestamp((AT - 86400) as i64).unwrap();
    p.not_after = OffsetDateTime::from_unix_timestamp(end as i64).unwrap();
    if ca {
        p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    } else {
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    }
    p
}

#[derive(Default)]
struct Case {
    expired_leaf: bool,
    expired_root: bool,
    future_leaf: bool,
    future_intermediate: bool,
    tamper_signature: bool,
    at_secs: Option<u64>,
    expired_intermediate: bool,
    pin_intermediate: bool,
    wrong_root: bool,
    missing_x5c: bool,
    outside_key: bool,
    es256: bool,
    claim: Option<(&'static str, Value)>,
}

async fn run(case: Case) -> Result<ModelIdentity, String> {
    let key = || KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
    let root_key = key();
    let root_params = params(
        "test root",
        true,
        if case.expired_root {
            AT - 1
        } else {
            AT + 10000
        },
    );
    let root = root_params.self_signed(&root_key).unwrap();
    let root_issuer = Issuer::new(root_params, &root_key);
    let int_key = key();
    let mut int_params = params(
        "test intermediate",
        true,
        if case.expired_intermediate {
            AT - 1
        } else {
            AT + 1000
        },
    );
    if case.future_intermediate {
        int_params.not_before = OffsetDateTime::from_unix_timestamp((AT + 1) as i64).unwrap();
    }
    let intermediate = int_params.signed_by(&int_key, &root_issuer).unwrap();
    let int_issuer = Issuer::new(int_params, &int_key);
    let leaf_key = key();
    let mut leaf_params = params(
        "test signer",
        false,
        if case.expired_leaf { AT - 1 } else { AT + 100 },
    );
    if case.future_leaf {
        leaf_params.not_before = OffsetDateTime::from_unix_timestamp((AT + 1) as i64).unwrap();
    }
    let leaf = leaf_params.signed_by(&leaf_key, &int_issuer).unwrap();
    let other = key();
    let signing_key = if case.outside_key { &other } else { &leaf_key };
    let public = signing_key.public_key_raw();
    let mut jwk = json!({"kid":"test", "kty":"EC", "crv":"P-384",
        "x": URL_SAFE_NO_PAD.encode(&public[1..49]), "y": URL_SAFE_NO_PAD.encode(&public[49..97])});
    if !case.missing_x5c {
        jwk["x5c"] = json!([
            STANDARD.encode(leaf.der()),
            STANDARD.encode(intermediate.der())
        ]);
    }
    let m: Value = serde_json::from_slice(&fixture("meta.json")).unwrap();
    let mut model: Value = serde_json::from_slice(&fixture("model-report.json")).unwrap();
    let payload: Value = serde_json::from_str(
        model["model_attestations"][0]["nvidia_payload"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let mut claims = json!({"iss":"https://nras.attestation.nvidia.com", "iat":AT - 1, "nbf":AT - 1, "exp":AT + 60,
        "eat_nonce":payload["nonce"], "x-nvidia-overall-att-result":true});
    if let Some((name, value)) = case.claim {
        claims[name] = value;
    }
    let mut header = Header::new(if case.es256 {
        Algorithm::ES256
    } else {
        Algorithm::ES384
    });
    header.kid = Some("test".into());
    let mut jwt = if case.es256 {
        let k = KeyPair::generate().unwrap();
        encode(
            &header,
            &claims,
            &EncodingKey::from_ec_der(&k.serialize_der()),
        )
        .unwrap()
    } else {
        encode(
            &header,
            &claims,
            &EncodingKey::from_ec_der(&signing_key.serialize_der()),
        )
        .unwrap()
    };
    if case.tamper_signature {
        let at = jwt.rfind('.').unwrap() + 1;
        let c = if &jwt[at..at + 1] == "A" { "B" } else { "A" };
        jwt.replace_range(at..at + 1, c);
    }
    let nras = Router::new()
        .route(
            "/attest",
            post(move || {
                let jwt = jwt.clone();
                async move { Json(json!([["JWT", jwt]])) }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let jwk = jwk.clone();
                async move { Json(json!({"keys":[jwk]})) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let nras_url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, nras).await.unwrap() });
    let fake = nymira_fake_near_ai::start("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    // Keep the report bytes bound by the real recorded quotes.
    let gateway: Value = serde_json::from_slice(&fixture("gateway-report.json")).unwrap();
    fake.handle
        .patch(&json!({"recorded_model_report": model.take(), "recorded_gateway_report":gateway}))
        .unwrap();
    let mut policy: Policy = serde_json::from_slice(&fixture("policy-draft.json")).unwrap();
    let wrong = params("other root", true, AT + 10000)
        .self_signed(&other)
        .unwrap();
    policy.nvidia_certificates = vec![NvidiaCertificate {
        der: STANDARD.encode(if case.wrong_root {
            wrong.der()
        } else if case.pin_intermediate {
            intermediate.der()
        } else {
            root.der()
        }),
        official_source_verified: false,
    }];
    let signer = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
    policy = policy.sign(&signer);
    policy = Policy::load(
        &serde_json::to_vec(&policy).unwrap(),
        &signer.verifying_key().to_bytes(),
        1,
    )
    .unwrap();
    let collateral = |name| Collateral::Recorded {
        collateral: Box::new(serde_json::from_slice(&fixture(name)).unwrap()),
        at_secs: AT,
    };
    let replay = Replay {
        model_collateral: collateral("collateral-model.json"),
        gateway_collateral: collateral("collateral-gateway.json"),
        gpu: None,
        nonce_model: m["nonce_model"].as_str().unwrap().into(),
        nonce_gateway: m["nonce_gateway"].as_str().unwrap().into(),
    };
    // Use the same signed-status check as the gateway; the certificate/JWT time is the signed ts.
    let signer = CompactSigner::from_seed(&Felt252::from_u64(91));
    let pk = signer.public_key();
    let signed_at = case.at_secs.unwrap_or(AT);
    let status = json!({"nonce":{"nonce":"test", "host":"op.test", "ts":signed_at},
        "nonce_sig":signer.sign(&nymira_operator::status::status_message("test", "op.test", signed_at))});
    let time = nymirad::gate::verify_status(
        &status,
        &json!({"x":pk.x.to_string(), "y":pk.y.to_string()}),
        signed_at,
    )
    .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let result = NearAiAttestor::replaying(policy, replay)
        .with_nras_endpoints(format!("{nras_url}/attest"), format!("{nras_url}/jwks"))
        .verify_model(&ReportRequest {
            gateway_url: &fake.url(),
            model: MODEL,
            nonce: "unused",
            http: &http,
            socks_url: None,
            at_secs: time.ts,
        })
        .await;
    task.abort();
    result
}

#[tokio::test]
async fn a_valid_nras_chain_passes_verify_model() {
    let id = run(Case::default()).await.unwrap();
    assert_eq!(
        id.verified_model.unwrap().gpu_evidence,
        nearai_inference_sdk::GpuEvidenceStatus::Verified
    );
}

#[tokio::test]
async fn an_intermediate_can_be_the_pinned_authority() {
    run(Case {
        pin_intermediate: true,
        ..Case::default()
    })
    .await
    .unwrap();
}

macro_rules! refused {
    ($name:ident, $case:expr, $reason:literal) => {
        #[tokio::test]
        async fn $name() {
            let error = run($case).await.unwrap_err();
            assert!(error.contains($reason), "{error}");
        }
    };
}
refused!(
    a_wrong_root_is_named,
    Case {
        wrong_root: true,
        ..Case::default()
    },
    "chain_wrong_root"
);
refused!(
    an_expired_leaf_is_named,
    Case {
        expired_leaf: true,
        ..Case::default()
    },
    "leaf_expired"
);
refused!(
    an_expired_intermediate_is_named,
    Case {
        expired_intermediate: true,
        ..Case::default()
    },
    "intermediate_expired"
);
refused!(
    an_expired_pinned_intermediate_is_named,
    Case {
        expired_intermediate: true,
        pin_intermediate: true,
        ..Case::default()
    },
    "intermediate_expired"
);
refused!(
    missing_x5c_is_named,
    Case {
        missing_x5c: true,
        ..Case::default()
    },
    "x5c_missing"
);
refused!(
    es256_is_named,
    Case {
        es256: true,
        ..Case::default()
    },
    "unsupported_algorithm"
);
refused!(
    a_signing_key_outside_the_chain_is_named,
    Case {
        outside_key: true,
        ..Case::default()
    },
    "signing_key_outside_chain"
);
refused!(
    a_wrong_nonce_is_named,
    Case {
        claim: Some(("eat_nonce", json!("99".repeat(32)))),
        ..Case::default()
    },
    "nonce_mismatch"
);
refused!(
    a_failed_overall_result_is_named,
    Case {
        claim: Some(("x-nvidia-overall-att-result", json!(false))),
        ..Case::default()
    },
    "gpu.attestation_rejected"
);
refused!(
    an_expired_token_is_named,
    Case {
        claim: Some(("exp", json!(AT))),
        ..Case::default()
    },
    "expired"
);
refused!(
    a_future_iat_is_refused,
    Case {
        claim: Some(("iat", json!(AT + 1))),
        ..Case::default()
    },
    "not_yet_valid"
);
refused!(
    a_future_nbf_is_refused,
    Case {
        claim: Some(("nbf", json!(AT + 1))),
        ..Case::default()
    },
    "not_yet_valid"
);
refused!(
    a_foreign_issuer_is_refused,
    Case {
        claim: Some(("iss", json!("https://attacker.invalid"))),
        ..Case::default()
    },
    "invalid_claims"
);

refused!(
    an_expired_pin_absent_from_x5c_is_refused,
    Case {
        expired_root: true,
        ..Case::default()
    },
    "intermediate_expired"
);
refused!(
    a_leaf_not_yet_valid_is_refused,
    Case {
        future_leaf: true,
        ..Case::default()
    },
    "leaf_not_yet_valid"
);
refused!(
    an_intermediate_not_yet_valid_is_refused,
    Case {
        future_intermediate: true,
        ..Case::default()
    },
    "intermediate_not_yet_valid"
);
refused!(
    a_modified_signature_is_refused,
    Case {
        tamper_signature: true,
        ..Case::default()
    },
    "invalid_signature"
);
refused!(
    a_changed_signed_time_expires_the_same_leaf,
    Case {
        at_secs: Some(AT + 101),
        ..Case::default()
    },
    "leaf_expired"
);
