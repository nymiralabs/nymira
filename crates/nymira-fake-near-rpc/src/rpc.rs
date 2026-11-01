//! JSON-RPC method dispatch.

use crate::state::{
    handler_error, invalid_params, parse_signed_tx, Chain, RpcResult, Submission, TxStatusSpec,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};

pub fn method_not_found(method: &str) -> Value {
    json!({
        "name": "REQUEST_VALIDATION_ERROR",
        "cause": {"name": "METHOD_NOT_FOUND", "info": {"method_name": method}},
        "code": -32601, "message": "Method not found", "data": method
    })
}

pub fn dispatch(c: &mut Chain, method: &str, p: &Value) -> RpcResult {
    match method {
        "query" => query(c, p),
        "block" => {
            let h = c.resolve_block(p)?;
            Ok(c.block_json(h))
        }
        "status" => Ok(c.status_json()),
        "gas_price" => Ok(json!({"gas_price": c.gas_price()})),
        "tx" | "EXPERIMENTAL_tx_status" => tx(c, p),
        "send_tx" | "broadcast_tx_commit" | "broadcast_tx_async" => send(c, method, p),
        _ => Err(method_not_found(method)),
    }
}

fn field<'a>(p: &'a Value, key: &str) -> Result<&'a str, Value> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params(format!("missing string field {key}")))
}

fn query(c: &Chain, p: &Value) -> RpcResult {
    let request_type = field(p, "request_type")?;
    if ![
        "call_function",
        "view_account",
        "view_access_key_list",
        "view_access_key",
    ]
    .contains(&request_type)
    {
        return Err(invalid_params(format!(
            "unsupported request_type {request_type}"
        )));
    }
    let h = c.resolve_block(p)?;
    let (block_height, block_hash) = (h, c.block_hash(h));
    let account = field(p, "account_id")?;
    c.require_account(account, h)?;
    let mut out = match request_type {
        "call_function" => {
            let method = field(p, "method_name")?;
            let args = STANDARD
                .decode(field(p, "args_base64")?)
                .map_err(|e| invalid_params(format!("args_base64: {e}")))?;
            match c.call_view(account, method, &args, h) {
                Ok(bytes) => json!({"result": bytes, "logs": []}),
                Err(msg) => json!({"error": msg, "logs": []}),
            }
        }
        "view_account" => {
            let a = c.account(account, h);
            json!({
                "amount": a.amount, "locked": a.locked, "code_hash": a.code_hash,
                "storage_usage": a.storage_usage, "storage_paid_at": 0
            })
        }
        "view_access_key_list" => {
            let keys: Vec<Value> = c
                .access_keys(account, h)
                .into_iter()
                .map(|k| json!({"public_key": k.public_key, "access_key": {"nonce": k.nonce, "permission": k.permission}}))
                .collect();
            json!({"keys": keys})
        }
        "view_access_key" => {
            let key = field(p, "public_key")?;
            let k = c.access_keys(account, h).into_iter().find(|k| k.public_key == key).ok_or_else(|| {
                handler_error(
                    "UNKNOWN_ACCESS_KEY",
                    json!({"block_hash": block_hash, "block_height": block_height, "public_key": key}),
                    format!("access key {key} does not exist while viewing at block #{h}"),
                )
            })?;
            json!({"nonce": k.nonce, "permission": k.permission})
        }
        other => return Err(invalid_params(format!("unsupported request_type {other}"))),
    };
    out["block_height"] = block_height.into();
    out["block_hash"] = block_hash.into();
    Ok(out)
}

fn unknown_tx(hash: &str) -> Value {
    handler_error(
        "UNKNOWN_TRANSACTION",
        json!({"requested_transaction_hash": hash}),
        format!("Transaction {hash} doesn't exist"),
    )
}

fn tx(c: &Chain, p: &Value) -> RpcResult {
    let hash = match p {
        Value::Array(a) => a.first().and_then(Value::as_str),
        Value::Object(_) => p.get("tx_hash").and_then(Value::as_str),
        _ => None,
    }
    .ok_or_else(|| invalid_params("missing transaction hash".into()))?;
    c.tx(hash)
        .map(|t| c.tx_json(t))
        .ok_or_else(|| unknown_tx(hash))
}

fn send(c: &mut Chain, method: &str, p: &Value) -> RpcResult {
    let b64 = match p {
        Value::Array(a) => a.first().and_then(Value::as_str),
        Value::Object(_) => p.get("signed_tx_base64").and_then(Value::as_str),
        _ => None,
    }
    .ok_or_else(|| invalid_params("missing signed transaction".into()))?;
    let bytes = STANDARD
        .decode(b64)
        .map_err(|e| invalid_params(format!("signed transaction: {e}")))?;
    let info = parse_signed_tx(&bytes)
        .ok_or_else(|| invalid_params("cannot parse the signed transaction".into()))?;
    c.submissions.push(Submission {
        method: method.into(),
        tx_base64: b64.into(),
        tx_hash: info.hash.clone(),
        signer_id: info.signer_id.clone(),
        receiver_id: info.receiver_id.clone(),
        nonce: info.nonce,
        wait_until: p
            .get("wait_until")
            .and_then(Value::as_str)
            .map(String::from),
        at_height: c.head(),
    });
    if method == "broadcast_tx_async" {
        return Ok(json!(info.hash));
    }
    let mut spec =
        TxStatusSpec::success(&info.hash, &info.signer_id, &info.receiver_id).at_height(c.head());
    spec.nonce = info.nonce;
    let stored = c.record_tx(spec);
    Ok(c.tx_json(&stored))
}
