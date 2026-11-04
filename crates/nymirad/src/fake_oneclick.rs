//! Local 1Click fixture. Payment invokes a sandbox transfer; it never moves real funds.
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use ed25519_dalek::Signer;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

#[async_trait::async_trait]
pub trait Delivery: Send + Sync {
    async fn deliver(&self, request: &Value) -> Result<(String, String), String>;
}
struct Data {
    delivery: Arc<dyn Delivery>,
    quotes: Mutex<BTreeMap<String, Value>>,
    statuses: Mutex<BTreeMap<String, Value>>,
    seen: std::sync::Mutex<Vec<Value>>,
    refused: std::sync::Mutex<Option<String>>,
}
pub struct Running {
    base: String,
    task: tokio::task::JoinHandle<()>,
    data: Arc<Data>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Running {
    pub fn url(&self) -> String {
        self.base.clone()
    }
    pub fn refuse_asset(&self, asset: &str) {
        *self.data.refused.lock().unwrap() = Some(asset.into());
    }
    pub fn requests(&self) -> Vec<Value> {
        self.data.seen.lock().unwrap().clone()
    }
}
async fn quote(
    State(data): State<Arc<Data>>,
    Json(request): Json<Value>,
) -> (StatusCode, Json<Value>) {
    data.seen.lock().unwrap().push(request.clone());
    if data.refused.lock().unwrap().as_deref() == request["originAsset"].as_str() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "source unavailable"})),
        );
    }
    let amount = request["amount"]
        .as_str()
        .and_then(|s| s.parse::<u128>().ok())
        .unwrap_or(0);
    let mut quote = json!({"amountIn": (amount + 15000).to_string(), "amountInFormatted": format!("{:.6}", (amount + 15000) as f64 / 1e6), "amountInUsd": "1.065", "minAmountIn": amount.to_string(), "amountOut": amount.to_string(), "amountOutFormatted": format!("{:.6}", amount as f64 / 1e6), "amountOutUsd": "1.05", "minAmountOut": amount.to_string()});
    if request["dry"] == false {
        let address = format!("fake-deposit-{}", crate::net::identity());
        quote["depositAddress"] = address.clone().into();
        quote["deadline"] = request["deadline"].clone();
        quote["timeEstimate"] = 1.into();
        data.quotes
            .lock()
            .await
            .insert(address.clone(), request.clone());
        data.statuses
            .lock()
            .await
            .insert(address, json!({"status": "PENDING_DEPOSIT"}));
    }
    let mut response =
        json!({"quoteRequest": request, "quote": quote, "timestamp": "2026-11-04T00:00:00.000Z"});
    let signature = crate::oneclick::mock_signer()
        .sign(crate::oneclick::quote_hash(&response).unwrap().as_bytes());
    response["signature"] = format!(
        "ed25519:{}",
        bs58::encode(signature.to_bytes()).into_string()
    )
    .into();
    (StatusCode::CREATED, Json(response))
}
async fn pay(State(data): State<Arc<Data>>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let address = body["depositAddress"].as_str().unwrap_or("");
    let request = data.quotes.lock().await.get(address).cloned();
    let Some(request) = request else {
        return (StatusCode::NOT_FOUND, Json(json!({})));
    };
    // Hold the payment lock through delivery, so a duplicate cannot transfer twice.
    let mut statuses = data.statuses.lock().await;
    if statuses[address]["status"] != "PENDING_DEPOSIT" {
        return (StatusCode::OK, Json(statuses[address].clone()));
    }
    match data.delivery.deliver(&request).await {
        Ok((hash, signer)) => {
            statuses.insert(address.into(), json!({"status": "SUCCESS", "swapDetails": {"nearTxHashes": [hash], "destinationSigner": signer}}));
        }
        Err(error) => {
            statuses.insert(address.into(), json!({"status": "FAILED", "error": error}));
        }
    }
    (StatusCode::OK, Json(statuses[address].clone()))
}
pub async fn start(delivery: Arc<dyn Delivery>) -> std::io::Result<Running> {
    let data = Arc::new(Data {
        delivery,
        quotes: Mutex::new(BTreeMap::new()),
        statuses: Mutex::new(BTreeMap::new()),
        seen: Default::default(),
        refused: Default::default(),
    });
    let app =
        Router::new()
            .route("/v0/quote", post(quote))
            .route("/mock/pay", post(pay))
            .route(
                "/v0/status",
                get(
                    |State(data): State<Arc<Data>>,
                     Query(query): Query<BTreeMap<String, String>>| async move {
                        data.seen
                            .lock()
                            .unwrap()
                            .push(json!({"statusQuery": query}));
                        Json(
                            data.statuses
                                .lock()
                                .await
                                .get(
                                    query
                                        .get("depositAddress")
                                        .map(String::as_str)
                                        .unwrap_or(""),
                                )
                                .cloned()
                                .unwrap_or(json!({"status": "FAILED"})),
                        )
                    },
                ),
            )
            .with_state(data.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Running { base, task, data })
}
