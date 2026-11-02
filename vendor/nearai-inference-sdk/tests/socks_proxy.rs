//! Nymira patch: every client the SDK builds for evidence reaches its service through a SOCKS5
//! proxy, by name. The proxy here accepts the CONNECT, records the destination and hangs up, so
//! each request fails after the proxy has seen where it was going. A client that skipped the
//! proxy would try the real internet and leave no record.

use std::sync::{Arc, Mutex};

use nearai_inference_sdk::{
    create_pinned_tls_client_via, AttestationClient, DefaultTdxQuoteVerifier, GpuEvidenceVerifier,
    NrasGpuEvidenceVerifier, TdxQuoteVerifier, DEFAULT_INTEL_PCCS_URL, DEFAULT_NVIDIA_NRAS_URL,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A SOCKS5 listener without authentication that records `host:port` of each CONNECT.
async fn proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "socks5h://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                s.read_exact(&mut head).await.ok()?;
                let mut methods = vec![0u8; head[1] as usize];
                s.read_exact(&mut methods).await.ok()?;
                s.write_all(&[5, 0]).await.ok()?;
                let mut req = [0u8; 4];
                s.read_exact(&mut req).await.ok()?;
                let host = match req[3] {
                    3 => {
                        let mut n = [0u8; 1];
                        s.read_exact(&mut n).await.ok()?;
                        let mut name = vec![0u8; n[0] as usize];
                        s.read_exact(&mut name).await.ok()?;
                        String::from_utf8_lossy(&name).into_owned()
                    }
                    other => format!("<address type {other}: the name was resolved locally>"),
                };
                let mut port = [0u8; 2];
                s.read_exact(&mut port).await.ok()?;
                log.lock()
                    .unwrap()
                    .push(format!("{host}:{}", u16::from_be_bytes(port)));
                // "Connection refused" ends the request.
                s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok()
            });
        }
    });
    (url, seen)
}

fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[tokio::test]
async fn the_cloud_api_client_and_its_gateway_binding_client_use_the_proxy() {
    let (url, log) = proxy().await;
    let api = AttestationClient::with_base_url("key".into(), "https://cloud-api.near.ai/v1")
        .unwrap()
        .with_socks_proxy(&url)
        .unwrap();
    let _ = api.fetch_model_metadata("zai-org/GLM-5").await;
    let _ = api.fetch_gateway_attestation(Default::default()).await;
    let names = seen(&log);
    assert!(names.len() >= 2, "{names:?}");
    assert!(
        names.iter().all(|n| n == "cloud-api.near.ai:443"),
        "{names:?}"
    );
}

#[tokio::test]
async fn the_pinned_client_uses_the_proxy() {
    let (url, log) = proxy().await;
    let client = create_pinned_tls_client_via(&["ab".repeat(32)], Some(&url)).unwrap();
    let _ = client
        .get("https://cloud-api.near.ai/v1/models")
        .send()
        .await;
    assert_eq!(seen(&log), ["cloud-api.near.ai:443"]);
}

#[tokio::test]
async fn nras_and_pccs_use_the_proxy() {
    let (url, log) = proxy().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&url).unwrap())
        .build()
        .unwrap();

    let nras = NrasGpuEvidenceVerifier::with_client(client.clone(), DEFAULT_NVIDIA_NRAS_URL);
    let _ = nras
        .verify(&format!(r#"{{"nonce":"{}"}}"#, "ab".repeat(32)))
        .await;
    // A structurally valid SGX quote that asks the PCCS for a PCK certificate.
    let mut auth = vec![0; 64 + 64 + 384 + 64];
    auth.extend(0_u16.to_le_bytes());
    auth.extend(2_u16.to_le_bytes());
    auth.extend(276_u32.to_le_bytes());
    auth.extend([0; 276]);
    let mut quote = vec![0; 48 + 384];
    quote[..2].copy_from_slice(&3_u16.to_le_bytes());
    quote.extend((auth.len() as u32).to_le_bytes());
    quote.extend(auth);
    let _ = DefaultTdxQuoteVerifier::new(DEFAULT_INTEL_PCCS_URL)
        .with_http_client(client)
        .verify(&hex::encode(quote))
        .await;

    let names = seen(&log);
    for expected in [
        "nras.attestation.nvidia.com:443",
        "api.trustedservices.intel.com:443",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} not in {names:?}"
        );
    }
    assert!(names.iter().all(|n| !n.starts_with('<')), "{names:?}");
}
