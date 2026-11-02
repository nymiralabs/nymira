//! JSON-RPC reads and the transport they go over.
//!
//! The client never follows redirects and never reads a proxy from the environment. It goes out
//! through a SOCKS5 proxy (Tor) or, only when the caller says so, directly. A managed Tor that is
//! not ready sends nothing.

use std::time::Duration;

use serde_json::{json, Value};

use crate::tor::{SharedTor, TorState};

#[derive(Clone, Debug)]
pub enum Transport {
    /// `host:port` of a loopback SOCKS5 proxy someone else runs; names are resolved by the proxy.
    Socks(String),
    /// The Tor this process manages. A state other than `Ready` sends nothing.
    Tor(SharedTor),
    /// Shows this machine's address to the endpoint. Demo and test builds, and nothing the
    /// user has not agreed to.
    Direct,
}

impl Transport {
    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct)
    }

    /// The SOCKS5 URL for `identity` (`socks5h://<identity>:x@127.0.0.1:port`), `None` on a direct
    /// transport. A managed Tor that is not ready is an error, so no caller builds a client for it.
    pub fn socks_url(&self, identity: &str) -> Result<Option<String>, RpcError> {
        match self {
            Self::Direct => Ok(None),
            Self::Socks(addr) => Ok(Some(format!("socks5h://{identity}:x@{addr}"))),
            Self::Tor(tor) => {
                tor.ensure_ready().map_err(RpcError::TorDown)?;
                Ok(Some(format!("socks5h://{identity}:x@{}", tor.socks_addr())))
            }
        }
    }

    /// The loopback SOCKS address, when there is one.
    pub fn socks_addr(&self) -> Option<String> {
        match self {
            Self::Socks(addr) => Some(addr.clone()),
            Self::Tor(tor) => Some(tor.socks_addr().to_string()),
            Self::Direct => None,
        }
    }

    /// Fails when a managed Tor is no longer ready. A client built while Tor was up keeps its
    /// proxy address; call this before each send on it, so a Tor that died (and whose port
    /// something else may now hold) gets no more traffic.
    pub fn ensure_ready(&self) -> Result<(), RpcError> {
        match self {
            Self::Tor(tor) => tor.ensure_ready().map_err(RpcError::TorDown),
            _ => Ok(()),
        }
    }

    /// The Tor line of `nymirad status`.
    pub fn status_line(&self) -> String {
        match self {
            Self::Tor(tor) => tor.status_line(),
            Self::Socks(addr) => {
                format!("Tor: external SOCKS proxy at {addr}, not managed or checked")
            }
            Self::Direct => "Tor: off (demo build, traffic is direct)".into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("rpc transport: {0}")]
    Transport(String),
    /// Tor is not running or not ready; nothing was sent.
    #[error("Tor is down: {0}")]
    TorDown(String),
    /// Tor works and the other side does not accept its exits.
    #[error("the other side refuses Tor: {0}")]
    TorRefused(String),
    #[error("rpc answered an error: {0}")]
    Node(String),
    #[error("rpc result did not parse: {0}")]
    Shape(String),
}

#[derive(Clone)]
pub struct Rpc {
    transport: Transport,
    url: String,
}

/// An HTTP client for `transport`: no redirects, no proxy from the environment. Over SOCKS the
/// client carries a fresh random identity, so Tor puts everything it sends on one circuit and
/// nothing else on that circuit. Keep the client for one conversation; build a new one for each
/// other connection.
pub fn client(transport: &Transport) -> Result<reqwest::Client, RpcError> {
    client_with_identity(transport, &crate::net::identity())
}

/// [`client`] with the SOCKS identity chosen by the caller, so a connection opened by hand
/// ([`crate::net::prewarm`]) lands on the same Tor circuit as this client's requests.
pub fn client_with_identity(
    transport: &Transport,
    identity: &str,
) -> Result<reqwest::Client, RpcError> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .no_proxy();
    let socks = |addr: &str| -> Result<reqwest::Proxy, RpcError> {
        reqwest::Proxy::all(format!("socks5h://{identity}:x@{addr}"))
            .map_err(|e| RpcError::Transport(e.to_string()))
    };
    let builder = match transport {
        Transport::Direct => builder,
        Transport::Socks(addr) => builder.proxy(socks(addr)?),
        Transport::Tor(tor) => match tor.state() {
            TorState::Ready => builder.proxy(socks(&tor.socks_addr().to_string())?),
            TorState::Starting(p) => {
                return Err(RpcError::TorDown(format!("still starting ({p}%)")))
            }
            TorState::Failed(why) => return Err(RpcError::TorDown(why)),
        },
    };
    builder
        .build()
        .map_err(|e| RpcError::Transport(e.to_string()))
}

/// What a failed send means for the user.
pub fn send_error(e: reqwest::Error) -> RpcError {
    match crate::net::classify(e) {
        crate::net::Failure::TorDown(m) => RpcError::TorDown(m),
        crate::net::Failure::TorRefused(m) => RpcError::TorRefused(m),
        crate::net::Failure::Other(m) => RpcError::Transport(m),
    }
}

impl Rpc {
    pub fn new(url: impl Into<String>, transport: &Transport) -> Result<Self, RpcError> {
        // A direct client is checked now, a Tor client when it is used: it may not be ready yet.
        if transport.is_direct() {
            client(transport)?;
        }
        Ok(Self {
            transport: transport.clone(),
            url: url.into(),
        })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": "nymirad", "method": method, "params": params });
        // One connection, one identity: a read never shares a circuit with another read.
        let http = client(&self.transport)?;
        let reply = http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(send_error)?;
        if !self.transport.is_direct() && crate::net::refuses_tor(reply.status().as_u16()) {
            return Err(RpcError::TorRefused(format!("status {}", reply.status())));
        }
        let reply: Value = reply
            .json()
            .await
            .map_err(|e| RpcError::Transport(e.without_url().to_string()))?;
        if let Some(error) = reply.get("error") {
            return Err(RpcError::Node(error.to_string()));
        }
        Ok(reply["result"].clone())
    }

    /// A view call at the final block, decoded from JSON.
    pub async fn view(&self, account: &str, method: &str, args: Value) -> Result<Value, RpcError> {
        self.view_at(json!({ "finality": "final" }), account, method, args)
            .await
    }

    /// The hash and height of the latest final block, to read several views at one block.
    pub async fn final_block(&self) -> Result<(String, u64), RpcError> {
        let block = self.call("block", json!({ "finality": "final" })).await?;
        match (
            block["header"]["hash"].as_str(),
            block["header"]["height"].as_u64(),
        ) {
            (Some(hash), Some(height)) => Ok((hash.into(), height)),
            _ => Err(RpcError::Shape("block header".into())),
        }
    }

    /// A view call at `block` (`{"finality": "final"}` or `{"block_id": "<hash>"}`).
    pub async fn view_at(
        &self,
        block: Value,
        account: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        use base64::Engine;
        let mut params = json!({
            "request_type": "call_function",
            "account_id": account, "method_name": method,
            "args_base64": base64::engine::general_purpose::STANDARD.encode(args.to_string()),
        });
        params.as_object_mut().expect("object").extend(
            block
                .as_object()
                .expect("block selector is an object")
                .clone(),
        );
        let result = self.call("query", params).await?;
        let bytes: Vec<u8> = result["result"]
            .as_array()
            .ok_or_else(|| RpcError::Shape(format!("no result for {method}: {result}")))?
            .iter()
            .map(|b| b.as_u64().unwrap_or(0) as u8)
            .collect();
        serde_json::from_slice(&bytes).map_err(|e| RpcError::Shape(format!("{method}: {e}")))
    }

    /// The vault's status for a note commitment, for diagnostics. It tells the RPC provider which
    /// note is yours, so it is asked over Tor only and the call is refused on a direct transport.
    pub async fn note_status(
        &self,
        vault: &str,
        commitment: &str,
    ) -> Result<Option<String>, RpcError> {
        if self.transport.is_direct() {
            return Err(RpcError::Transport(
                "note_status is called over Tor only".into(),
            ));
        }
        let status = self
            .view(vault, "note_status", json!({ "commitment": commitment }))
            .await?;
        Ok(status.as_str().map(String::from))
    }

    /// The deployed code hash of an account, base58 as the node prints it.
    pub async fn code_hash(&self, account: &str) -> Result<String, RpcError> {
        let result = self
            .call(
                "query",
                json!({ "request_type": "view_account", "finality": "final", "account_id": account }),
            )
            .await?;
        result["code_hash"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| RpcError::Shape("no code_hash".into()))
    }

    /// Every access key of an account as `{public_key, permission}`, sorted by key.
    pub async fn access_keys(&self, account: &str) -> Result<Vec<Value>, RpcError> {
        let result = self
            .call(
                "query",
                json!({ "request_type": "view_access_key_list", "finality": "final", "account_id": account }),
            )
            .await?;
        let mut keys: Vec<Value> = result["keys"]
            .as_array()
            .ok_or_else(|| RpcError::Shape("no keys".into()))?
            .iter()
            .map(|k| json!({ "public_key": k["public_key"], "permission": k["access_key"]["permission"] }))
            .collect();
        keys.sort_by_key(|k| k["public_key"].as_str().unwrap_or("").to_string());
        Ok(keys)
    }
}
