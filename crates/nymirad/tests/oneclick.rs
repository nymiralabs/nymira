//! The 1Click HTTP client against a stub on loopback: the request it sends and how it reads the
//! answers. The SOCKS route and the live service are not covered here.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use nymirad::quotes::{OneClick, QuoteError, QuoteSource, SHORTLIST};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<Value>>>;

async fn stub(status: u16, reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route(
            "/v0/quote",
            post(
                move |State(seen): State<Seen>, Json(body): Json<Value>| async move {
                    seen.lock().unwrap().push(body);
                    (StatusCode::from_u16(status).unwrap(), Json(reply.clone()))
                },
            ),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

fn source(base: &str) -> OneClick {
    OneClick::over(reqwest::Client::new(), base)
}

#[tokio::test]
async fn a_dry_quote_request_carries_no_user_data_and_the_amount_is_read_back() {
    let (base, seen) = stub(
        200,
        json!({"quote": {"amountOutFormatted": "0.9987", "amountOut": "998700"}}),
    )
    .await;
    let out = source(&base).quote(&SHORTLIST[0]).await.unwrap();
    assert_eq!(out, "0.9987");
    let body = seen.lock().unwrap()[0].clone();
    assert_eq!(body["dry"], true);
    assert_eq!(body["swapType"], "EXACT_INPUT");
    assert_eq!(body["amount"], "1000000");
    assert_eq!(body["originAsset"], SHORTLIST[0].id);
    let zeros = "0".repeat(64);
    assert_eq!(body["refundTo"], zeros.as_str());
    assert_eq!(body["recipient"], zeros.as_str());
}

#[tokio::test]
async fn the_raw_amount_stands_in_when_the_formatted_one_is_missing() {
    let (base, _) = stub(200, json!({"quote": {"amountOut": "998700"}})).await;
    assert_eq!(source(&base).quote(&SHORTLIST[0]).await.unwrap(), "998700");
}

#[tokio::test]
async fn a_tor_refusal_and_other_failures_are_told_apart() {
    for status in [403, 451] {
        let (base, _) = stub(status, json!({})).await;
        assert_eq!(
            source(&base).quote(&SHORTLIST[0]).await.unwrap_err(),
            QuoteError::TorRefused
        );
    }
    let (base, _) = stub(500, json!({})).await;
    assert_eq!(
        source(&base).quote(&SHORTLIST[0]).await.unwrap_err(),
        QuoteError::Failed("status 500".into())
    );
    let (base, _) = stub(200, json!({"quote": {}})).await;
    assert!(matches!(
        source(&base).quote(&SHORTLIST[0]).await.unwrap_err(),
        QuoteError::Failed(_)
    ));
}
