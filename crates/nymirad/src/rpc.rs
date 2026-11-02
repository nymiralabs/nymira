//! JSON-RPC reads and the transport they go over.
//!
//! The client never follows redirects and never reads a proxy from the environment. It goes out
//! through a SOCKS5 proxy (Tor) or, only when the caller says so, directly.

use std::time::Duration;

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// `host:port` of a loopback SOCKS5 proxy; names are resolved by the proxy.
    Socks(String),
    /// Shows this machine's address to the endpoint. The caller asks the user first.
    Direct,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("rpc transport: {0}")]
    Transport(String),
    #[error("rpc answered an error: {0}")]
    Node(String),
    #[error("rpc result did not parse: {0}")]
    Shape(String),
}

#[derive(Clone)]
pub struct Rpc {
    http: reqwest::Client,
    url: String,
}

/// An HTTP client for `transport`: no redirects, no proxy from the environment.
pub fn client(transport: &Transport) -> Result<reqwest::Client, RpcError> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .no_proxy();
    let builder = match transport {
        Transport::Direct => builder,
        Transport::Socks(addr) => builder.proxy(
            reqwest::Proxy::all(format!("socks5h://{addr}"))
                .map_err(|e| RpcError::Transport(e.to_string()))?,
        ),
    };
    builder
        .build()
        .map_err(|e| RpcError::Transport(e.to_string()))
}

impl Rpc {
    pub fn new(url: impl Into<String>, transport: &Transport) -> Result<Self, RpcError> {
        Ok(Self {
            http: client(transport)?,
            url: url.into(),
        })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": "nymirad", "method": method, "params": params });
        let reply: Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcError::Transport(e.without_url().to_string()))?
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
