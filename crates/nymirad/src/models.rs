//! The pinned model list against NEAR AI's live list.

use nymira_operator::proxy::ModelSpec;
use serde_json::Value;

/// Nano-USD per token from the gateway's `{"amount", "scale"}` price: exact, or no price (a
/// price that does not convert exactly is treated as unreadable, as the operator does).
fn nano_per_token(price: &Value) -> Option<u64> {
    let amount = price["amount"].as_u64()?;
    let scale = price["scale"].as_u64()?;
    if scale <= 9 {
        amount.checked_mul(10u64.checked_pow(u32::try_from(9 - scale).ok()?)?)
    } else {
        let div = 10u64.checked_pow(u32::try_from(scale - 9).ok()?)?;
        (amount % div == 0).then_some(amount / div)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Live {
    pub id: String,
    pub provider_type: String,
    pub attestation_supported: bool,
    pub input_nano: u64,
    pub output_nano: u64,
}

/// `GET {gateway}/v1/model/list` (public, no key), all pages.
pub async fn fetch_live(http: &reqwest::Client, gateway: &str) -> Result<Vec<Live>, String> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let body: Value = http
            .get(format!("{}/v1/model/list", gateway.trim_end_matches('/')))
            .query(&[("limit", 1000usize), ("offset", offset)])
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?
            .json()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let page = body["models"].as_array().cloned().unwrap_or_default();
        for m in &page {
            if let (Some(id), Some(i), Some(o)) = (
                m["modelId"].as_str(),
                nano_per_token(&m["inputCostPerToken"]),
                nano_per_token(&m["outputCostPerToken"]),
            ) {
                out.push(Live {
                    id: id.into(),
                    provider_type: m["metadata"]["providerType"].as_str().unwrap_or("").into(),
                    attestation_supported: m["metadata"]["attestationSupported"] == true,
                    input_nano: i,
                    output_nano: o,
                });
            }
        }
        offset += page.len();
        if page.is_empty() || offset as u64 >= body["total"].as_u64().unwrap_or(0) {
            return Ok(out);
        }
    }
}

/// What differs between the pinned models and the live list; empty when they agree.
pub fn compare(pinned: &[ModelSpec], live: &[Live]) -> Vec<String> {
    let mut out = Vec::new();
    for m in pinned {
        match live.iter().find(|l| l.id == m.id) {
            None => out.push(format!("{} is no longer listed", m.id)),
            Some(l) => {
                if l.provider_type != "vllm" || !l.attestation_supported {
                    out.push(format!("{} is no longer an attested model", m.id));
                }
                if l.input_nano != m.input_nano_per_token
                    || l.output_nano != m.output_nano_per_token
                {
                    out.push(format!(
                        "{} price changed to {}/{} nano-USD per token (pinned {}/{})",
                        m.id,
                        l.input_nano,
                        l.output_nano,
                        m.input_nano_per_token,
                        m.output_nano_per_token
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pinned() -> Vec<ModelSpec> {
        vec![ModelSpec {
            id: "a".into(),
            input_nano_per_token: 80,
            output_nano_per_token: 500,
            output_cap_tokens: 4096,
        }]
    }

    fn live(provider: &str, attested: bool, i: u64, o: u64) -> Live {
        Live {
            id: "a".into(),
            provider_type: provider.into(),
            attestation_supported: attested,
            input_nano: i,
            output_nano: o,
        }
    }

    #[test]
    fn prices_convert_to_nano() {
        assert_eq!(nano_per_token(&json!({"amount": 80, "scale": 9})), Some(80));
        assert_eq!(nano_per_token(&json!({"amount": 8, "scale": 8})), Some(80));
        assert_eq!(nano_per_token(&json!({"amount": 5, "scale": 7})), Some(500));
    }

    #[test]
    fn an_unchanged_list_agrees_and_each_change_is_named() {
        assert!(compare(&pinned(), &[live("vllm", true, 80, 500)]).is_empty());
        assert!(compare(&pinned(), &[])[0].contains("no longer listed"));
        assert!(compare(&pinned(), &[live("other", true, 80, 500)])[0].contains("attested"));
        assert!(compare(&pinned(), &[live("vllm", false, 80, 500)])[0].contains("attested"));
        assert!(compare(&pinned(), &[live("vllm", true, 90, 500)])[0].contains("price changed"));
    }
}
