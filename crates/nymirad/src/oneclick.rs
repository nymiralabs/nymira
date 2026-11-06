//! 1Click quote signatures. The manager key is compiled in; a rotation needs a new release.
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const MANAGER_KEY: &str = "ed25519:reYaWhvwu8Jzo3WUM3zhn6VrhuMEF4eADL17qtRVifc";

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let ordered: std::collections::BTreeMap<_, _> = map.into_iter().collect();
            Value::Object(ordered.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        v => v,
    }
}

/// The SDK's signed field selection, followed by stable JSON, SHA-256 and base58.
pub fn quote_hash(response: &Value) -> Result<String, String> {
    let request = &response["quoteRequest"];
    let quote = &response["quote"];
    let mut data = Map::new();
    for name in [
        "dry",
        "swapType",
        "slippageTolerance",
        "originAsset",
        "depositType",
        "destinationAsset",
        "amount",
        "refundTo",
        "refundType",
        "recipient",
        "recipientType",
        "deadline",
    ] {
        let v = request
            .get(name)
            .ok_or_else(|| format!("missing quoteRequest.{name}"))?;
        data.insert(name.into(), v.clone());
    }
    for name in [
        "quoteWaitingTimeMs",
        "referral",
        "virtualChainRecipient",
        "virtualChainRefundRecipient",
        "customRecipientMsg",
    ] {
        if let Some(v) = request.get(name).filter(|v| truthy(v)) {
            data.insert(name.into(), v.clone());
        }
    }
    for name in [
        "amountIn",
        "amountInFormatted",
        "amountInUsd",
        "minAmountIn",
        "amountOut",
        "amountOutFormatted",
        "amountOutUsd",
        "minAmountOut",
    ] {
        let v = quote
            .get(name)
            .ok_or_else(|| format!("missing quote.{name}"))?;
        data.insert(name.into(), v.clone());
    }
    if request["dry"] == false {
        for name in [
            "depositAddress",
            "depositMemo",
            "deadline",
            "timeWhenInactive",
            "timeEstimate",
            "refundFee",
            "withdrawFee",
        ] {
            if let Some(v) = quote.get(name).filter(|v| truthy(v)) {
                data.insert(name.into(), v.clone());
            }
        }
    }
    data.insert(
        "timestamp".into(),
        response
            .get("timestamp")
            .filter(|v| v.is_string())
            .ok_or("missing timestamp")?
            .clone(),
    );
    let bytes = serde_json::to_vec(&sorted(Value::Object(data))).map_err(|e| e.to_string())?;
    Ok(bs58::encode(Sha256::digest(bytes)).into_string())
}

fn decode(value: &str) -> Result<Vec<u8>, String> {
    bs58::decode(value.strip_prefix("ed25519:").unwrap_or(value))
        .into_vec()
        .map_err(|_| "invalid ed25519 base58".into())
}

pub fn verify_signature(response: &Value) -> Result<(), String> {
    verify_with_key(response, MANAGER_KEY)
}

fn verify_with_key(response: &Value, key: &str) -> Result<(), String> {
    let bytes: [u8; 32] = decode(key)?.try_into().map_err(|_| "invalid manager key")?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| "invalid manager key")?;
    let sig = Signature::from_slice(&decode(
        response["signature"].as_str().ok_or("missing signature")?,
    )?)
    .map_err(|_| "invalid signature")?;
    key.verify_strict(quote_hash(response)?.as_bytes(), &sig)
        .map_err(|_| "quote signature does not verify".into())
}

pub const DIRECT_QUESTION: &str = "1Click calls do not go through Tor, because 1Click's terms forbid using Tor to get around its restrictions. 1Click will see your IP address, source wallet, commitment and payout account. Allow direct quote and status calls for this funding attempt? Type yes to continue: ";

/// One approved attempt. This capability is never serialized into the quote record or backup.
pub struct FundingClient {
    direct: crate::direct::DirectClient,
    last: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    mock_key: bool,
}

impl FundingClient {
    pub fn new(mode: crate::mode::Mode, confirmed: bool) -> Result<Self, String> {
        Self::build(crate::quotes::BASE_URL, mode, confirmed, false)
    }

    #[cfg(feature = "mock")]
    pub fn mock(base: &str, mode: crate::mode::Mode, confirmed: bool) -> Result<Self, String> {
        Self::build(base, mode, confirmed, true)
    }

    fn build(
        base: &str,
        mode: crate::mode::Mode,
        confirmed: bool,
        mock_key: bool,
    ) -> Result<Self, String> {
        if !mode.intents_enabled() {
            return Err("Intents has no testnet".into());
        }
        if !confirmed {
            return Err("1Click direct access was not confirmed".into());
        }
        Ok(Self {
            direct: crate::direct::DirectClient::new(base).map_err(|e| e.to_string())?,
            last: tokio::sync::Mutex::new(None),
            mock_key,
        })
    }

    async fn slot(&self) -> tokio::sync::MutexGuard<'_, Option<tokio::time::Instant>> {
        let last = self.last.lock().await;
        if let Some(at) = *last {
            tokio::time::sleep_until(at + std::time::Duration::from_secs(5)).await;
        }
        last
    }

    pub async fn quote(&self, request: &Value) -> Result<Value, String> {
        let mut last = self.slot().await;
        *last = Some(tokio::time::Instant::now());
        let (status, body) = self
            .direct
            .post_json("/v0/quote", request)
            .await
            .map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!("1Click quote HTTP {status}"));
        }
        self.verify(&body)?;
        Ok(body)
    }

    pub async fn status(&self, address: &str, memo: Option<&str>) -> Result<Value, String> {
        let mut last = self.slot().await;
        *last = Some(tokio::time::Instant::now());
        let mut query = vec![("depositAddress", address)];
        if let Some(memo) = memo {
            query.push(("depositMemo", memo));
        }
        let (status, body) = self
            .direct
            .get_json("/v0/status", &query)
            .await
            .map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!("1Click status HTTP {status}"));
        }
        Ok(body)
    }

    pub fn verify(&self, response: &Value) -> Result<(), String> {
        #[cfg(feature = "mock")]
        if self.mock_key {
            let key = mock_signer().verifying_key();
            return verify_with_key(response, &bs58::encode(key.as_bytes()).into_string());
        }
        let _ = self.mock_key;
        verify_signature(response)
    }

    pub fn names_asked(&self) -> Vec<String> {
        self.direct.names_asked()
    }
}

#[cfg(feature = "mock")]
pub fn mock_signer() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[44; 32])
}

/// Checks the response against locally held intent before exposing any payment address.
pub fn validate_quote(response: &Value, request: &Value, now: u64) -> Result<(), String> {
    for value in [request.get("deadline"), response["quote"].get("deadline")]
        .into_iter()
        .flatten()
    {
        let at = chrono::DateTime::parse_from_rfc3339(value.as_str().ok_or("invalid deadline")?)
            .map_err(|_| "invalid deadline")?
            .timestamp();
        if at <= i64::try_from(now).unwrap_or(i64::MAX) {
            return Err("quote expired; request a new quote before paying".into());
        }
    }
    for (name, value) in request
        .as_object()
        .ok_or("quote request is not an object")?
    {
        if response["quoteRequest"].get(name) != Some(value) {
            return Err(format!("quote request field {name} changed"));
        }
    }
    if response["quote"]["amountOut"] != request["amount"] {
        return Err("quote amountOut does not match denomination plus note fee".into());
    }
    Ok(())
}

/// Two-step path: the quote must pay the user's own account and carry no message.
pub fn validate_two_step(response: &Value, account: &str, vault: &str) -> Result<(), String> {
    let request = &response["quoteRequest"];
    if request["recipient"] == vault {
        return Err("a two-step quote must not name the vault as recipient".into());
    }
    if request["recipient"] != account {
        return Err("the quote does not pay your own NEAR account".into());
    }
    match request.get("customRecipientMsg") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(m)) if m.is_empty() => Ok(()),
        _ => Err("a two-step quote must carry no custom message".into()),
    }
}

#[cfg(feature = "mock")]
impl FundingClient {
    pub async fn mock_pay(&self, address: &str) -> Result<(), String> {
        if !self.mock_key {
            return Err("mock payment needs the mock endpoint".into());
        }
        let (status, _) = self
            .direct
            .post_json("/mock/pay", &serde_json::json!({"depositAddress": address}))
            .await
            .map_err(|e| e.to_string())?;
        if status != 200 {
            return Err(format!("mock payment HTTP {status}"));
        }
        Ok(())
    }
}

#[cfg(feature = "mock")]
pub(crate) fn verify_mock_signature(response: &Value) -> Result<(), String> {
    verify_with_key(
        response,
        &bs58::encode(mock_signer().verifying_key().as_bytes()).into_string(),
    )
}
