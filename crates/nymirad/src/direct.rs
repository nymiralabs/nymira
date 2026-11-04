//! The direct client, for the sends that must not wait for Tor: a confirmed close or exit sent
//! after the user agreed to show their address, and later real Intents calls on the mainnet
//! profile. It shares no code path with the Tor client (`rpc::client`), so a flag on one cannot
//! change the other.
//!
//! It talks to one host, taken from the compiled base URL. It never follows a redirect, never
//! reads a proxy from the environment, takes a path and not a URL, and resolves no name other
//! than that host.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum DirectError {
    #[error("base URL: {0}")]
    BaseUrl(String),
    #[error("the path {0:?} is not a path on the base URL's host")]
    Path(String),
    #[error("direct send failed: {0}")]
    Send(String),
}

/// Resolves the one allowed host and refuses every other name. Every name asked about is kept.
struct AllowedHost {
    host: String,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Resolve for AllowedHost {
    fn resolve(&self, name: Name) -> Resolving {
        let name = name.as_str().to_string();
        self.asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(name.clone());
        let allowed = name.eq_ignore_ascii_case(&self.host);
        Box::pin(async move {
            if !allowed {
                return Err(format!("{name} is not the host of this client").into());
            }
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            Ok(Box::new(found.into_iter()) as Addrs)
        })
    }
}

pub struct DirectClient {
    http: reqwest::Client,
    base: reqwest::Url,
    asked: Arc<Mutex<Vec<String>>>,
}

impl DirectClient {
    /// A client for `base`, which carries the scheme, the host and optionally a port.
    pub fn new(base: &str) -> Result<Self, DirectError> {
        let base = reqwest::Url::parse(base).map_err(|e| DirectError::BaseUrl(e.to_string()))?;
        if !matches!(base.scheme(), "https" | "http") {
            return Err(DirectError::BaseUrl("only http and https".into()));
        }
        let host = base
            .host_str()
            .ok_or_else(|| DirectError::BaseUrl("no host".into()))?
            .to_string();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(AllowedHost {
                host,
                asked: asked.clone(),
            }))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| DirectError::Send(e.to_string()))?;
        Ok(Self { http, base, asked })
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, DirectError> {
        // `//host/x` and `http://host/x` would move the request to another host.
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Err(DirectError::Path(path.into()));
        }
        let url = self
            .base
            .join(path)
            .map_err(|_| DirectError::Path(path.into()))?;
        if url.host_str() != self.base.host_str() || url.port() != self.base.port() {
            return Err(DirectError::Path(path.into()));
        }
        Ok(url)
    }

    /// POSTs JSON and returns the status and the body. A 3xx is returned as it is.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<(u16, Value), DirectError> {
        let reply = self
            .http
            .post(self.url(path)?)
            .json(body)
            .send()
            .await
            .map_err(|e| DirectError::Send(e.without_url().to_string()))?;
        let status = reply.status().as_u16();
        let body = reply.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    pub async fn get_json(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<(u16, Value), DirectError> {
        let reply = self
            .http
            .get(self.url(path)?)
            .query(query)
            .send()
            .await
            .map_err(|e| DirectError::Send(e.without_url().to_string()))?;
        let status = reply.status().as_u16();
        Ok((status, reply.json().await.unwrap_or(Value::Null)))
    }

    /// The underlying client, for a caller that builds its own requests (the NEAR transaction
    /// sender). It keeps this client's rules: one resolvable host, no redirect, no proxy.
    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    /// Every host name this client was asked to resolve.
    pub fn names_asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
