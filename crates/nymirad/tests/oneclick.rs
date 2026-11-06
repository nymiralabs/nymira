//! The quote-only HTTP boundary: placeholders, signed exact-output answers and Tor refusal.
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use ed25519_dalek::Signer;
use nymirad::quotes::{OneClick, QuoteError, QuoteSource, SHORTLIST};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
type Seen = Arc<Mutex<Vec<Value>>>;
async fn stub(status: u16, bad: bool) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new().route("/v0/quote", post(move |State(seen): State<Seen>, Json(body): Json<Value>| async move {
        seen.lock().unwrap().push(body.clone());
        let mut response = json!({"quoteRequest": body, "quote": {"amountIn": "1065000", "amountInFormatted": "1.065", "amountInUsd": "1.065", "minAmountIn": "1050000", "amountOut": body["amount"], "amountOutFormatted": "1.05", "amountOutUsd": "1.05", "minAmountOut": body["amount"]}, "timestamp": "2026-11-04T00:00:00.000Z"});
        let sig = nymirad::oneclick::mock_signer().sign(nymirad::oneclick::quote_hash(&response).unwrap().as_bytes());
        response["signature"] = format!("ed25519:{}", bs58::encode(sig.to_bytes()).into_string()).into();
        if bad { response["quoteRequest"]["recipient"] = "other.near".into(); }
        (StatusCode::from_u16(status).unwrap(), Json(response))
    })).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}
#[tokio::test]
async fn dry_quotes_use_only_mainnet_placeholders_and_exact_output() {
    let (base, seen) = stub(200, false).await;
    let source = OneClick::over(reqwest::Client::new(), &base);
    assert_eq!(source.quote(&SHORTLIST[0]).await.unwrap(), "1.065");
    let body = seen.lock().unwrap()[0].clone();
    assert_eq!(body["dry"], true);
    assert_eq!(body["swapType"], "EXACT_OUTPUT");
    assert_eq!(body["recipient"], "nymira.near");
    assert_eq!(
        body["refundTo"],
        "0x1111111111111111111111111111111111111111"
    );
    assert_eq!(body["destinationAsset"], nymirad::intents::DESTINATION);
    assert_eq!(
        body["customRecipientMsg"],
        nymirad::intents::placeholder_message()
    );
}
#[tokio::test]
async fn an_unsigned_change_never_becomes_a_quote_only_listing() {
    let (base, _) = stub(200, true).await;
    assert!(OneClick::over(reqwest::Client::new(), &base)
        .quote(&SHORTLIST[0])
        .await
        .is_err());
}
#[tokio::test]
async fn tor_refusals_and_other_failures_are_told_apart() {
    for status in [403, 451, 500] {
        let (base, _) = stub(status, false).await;
        let error = OneClick::over(reqwest::Client::new(), &base)
            .quote(&SHORTLIST[0])
            .await
            .unwrap_err();
        if status == 500 {
            assert!(matches!(error, QuoteError::Failed(_)));
        } else {
            assert_eq!(error, QuoteError::TorRefused);
        }
    }
}
