//! The NRAS relay: two calls to NVIDIA's attestation service, forwarded for clients whose Tor
//! exit NVIDIA turns away (issue #165).
//!
//! The relay is not a proxy. It has two routes, each tied to one fixed NVIDIA URL taken from the
//! operator's configuration, never from the request: the evidence POST and the JWKS GET. The
//! status and body of NVIDIA's answer go back unchanged. The relay adds no logic and is not
//! trusted: the client checks the token's signature and certificate chain itself.
//!
//! The relay writes nothing to disk or to the log, and keeps no request body past the call. It
//! sees no IP address (the public listener does not pass one on). What it learns is that some
//! client asked for a GPU check at some time.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use tokio::sync::Semaphore;

use crate::error::{ApiError, Code};

pub const NRAS_URL: &str = "https://nras.attestation.nvidia.com/v3/attest/gpu";
pub const JWKS_URL: &str = "https://nras.attestation.nvidia.com/.well-known/jwks.json";

/// The largest evidence payload accepted. A real one is about 100 KB.
pub const EVIDENCE_BODY_LIMIT: usize = 256 * 1024;
/// Calls a minute across all callers: the relay needs no identity, and a chat needs one pair.
pub const RELAYS_PER_MINUTE: usize = 60;
/// Calls to NVIDIA in flight at once.
pub const MAX_IN_FLIGHT: usize = 8;
/// The JWKS answer is reused this long.
pub const JWKS_CACHE_SECS: u64 = 30;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(20);
/// The alarm raised while NVIDIA does not answer the relay.
pub const ALARM_KEY: &str = "nras_relay_failing";

/// The two NVIDIA URLs. Tests point them at a local stand-in.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub evidence_url: String,
    pub jwks_url: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            evidence_url: NRAS_URL.into(),
            jwks_url: JWKS_URL.into(),
        }
    }
}

pub struct NrasRelay {
    cfg: RelayConfig,
    client: reqwest::Client,
    slots: Semaphore,
    recent: Mutex<VecDeque<u64>>,
    jwks: Mutex<Option<(u64, StatusCode, Bytes)>>,
}

impl NrasRelay {
    pub fn new(cfg: RelayConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(UPSTREAM_TIMEOUT)
                .build()
                .expect("client builds"),
            slots: Semaphore::new(MAX_IN_FLIGHT),
            recent: Mutex::new(VecDeque::new()),
            jwks: Mutex::new(None),
        }
    }

    /// A slot for one call to NVIDIA, then one count against the minute. A call turned away for
    /// either reason uses up neither: the slot is given back when the count is refused, and the
    /// count is taken only once the slot is held.
    fn admit(&self, now: u64) -> Result<tokio::sync::SemaphorePermit<'_>, ApiError> {
        let slot = self.slots.try_acquire().map_err(|_| {
            ApiError::new(Code::OperatorCapacity, "relay calls in flight").retry_after(1)
        })?;
        self.count(now)?;
        Ok(slot)
    }

    /// Counts one call against the minute, or says how long to wait.
    fn count(&self, now: u64) -> Result<(), ApiError> {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        while recent.front().is_some_and(|t| *t + 60 <= now) {
            recent.pop_front();
        }
        if recent.len() >= RELAYS_PER_MINUTE {
            let wait = recent
                .front()
                .map_or(1, |t| (*t + 60).saturating_sub(now).max(1));
            return Err(
                ApiError::new(Code::OperatorCapacity, "too many relay calls this minute")
                    .retry_after(wait),
            );
        }
        recent.push_back(now);
        Ok(())
    }
}

/// A JSON object with a `nonce` of 64 hex characters and an `evidence_list` array: the shape of
/// what NRAS takes. Nothing else is forwarded.
fn is_evidence_payload(body: &[u8]) -> bool {
    let Ok(serde_json::Value::Object(o)) = serde_json::from_slice(body) else {
        return false;
    };
    o.get("nonce")
        .and_then(|n| n.as_str())
        .is_some_and(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
        && o.get("evidence_list").is_some_and(|l| l.is_array())
}

/// Whether an NVIDIA answer says the relay itself cannot get through: the service is down, or it
/// turns the operator's address away. A 400 for a bad payload is the caller's, not an outage.
fn upstream_trouble(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::UNAUTHORIZED
        || status == StatusCode::TOO_MANY_REQUESTS
}

impl crate::operator::Operator {
    /// Forwards one evidence payload to NVIDIA. `body` has passed the size cap; this checks that
    /// it is a JSON object, then returns NVIDIA's status and body as they came.
    pub async fn relay_evidence(&self, body: Bytes) -> Result<(StatusCode, Bytes), ApiError> {
        // Checked before the call counts: what is not an evidence payload never reaches NVIDIA and
        // never uses up the limit.
        if !is_evidence_payload(&body) {
            return Err(ApiError::new(
                Code::MalformedRequest,
                "the body is not a GPU evidence payload",
            ));
        }
        let relay = &self.nras;
        let _slot = relay.admit(self.clock.now_secs())?;
        let sent = relay
            .client
            .post(&relay.cfg.evidence_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        self.relay_result(sent).await
    }

    /// NVIDIA's key set, from the cache when it is under [`JWKS_CACHE_SECS`] old. Only a 200 is
    /// cached.
    pub async fn relay_jwks(&self) -> Result<(StatusCode, Bytes), ApiError> {
        let relay = &self.nras;
        let now = self.clock.now_secs();
        if let Some((at, status, body)) = relay
            .jwks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if now < at + JWKS_CACHE_SECS {
                return Ok((*status, body.clone()));
            }
        }
        let _slot = relay.admit(now)?;
        let sent = relay.client.get(&relay.cfg.jwks_url).send().await;
        let (status, body) = self.relay_result(sent).await?;
        if status == StatusCode::OK {
            *relay.jwks.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((now, status, body.clone()));
        }
        Ok((status, body))
    }

    async fn trouble(&self, detail: &str) {
        self.alarms
            .raise(ALARM_KEY, "The NRAS relay cannot reach NVIDIA", detail)
            .await;
    }

    /// Turns NVIDIA's answer into the relay's, and raises or clears the alarm.
    async fn relay_result(
        &self,
        sent: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<(StatusCode, Bytes), ApiError> {
        let down = "NVIDIA did not answer the relay.";
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                self.trouble(down).await;
                return Err(if e.is_timeout() {
                    ApiError::new(Code::ProviderTimeout, "NVIDIA did not answer")
                } else {
                    ApiError::new(Code::ProviderError, "NVIDIA is unreachable")
                });
            }
        };
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let Ok(body) = response.bytes().await else {
            self.trouble(down).await;
            return Err(ApiError::new(Code::ProviderError, "NVIDIA is unreachable"));
        };
        if upstream_trouble(status) {
            self.trouble(&format!(
                "NVIDIA answered the relay with status {}.",
                status.as_u16()
            ))
            .await;
        } else if status.is_success() {
            self.alarms.resolve(ALARM_KEY);
        }
        Ok((status, body))
    }
}
