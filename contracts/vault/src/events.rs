//! NEP-297 events under the `nymira` standard.

use near_sdk::env;
use near_sdk::serde_json::{json, Value};

pub fn emit(event: &str, data: Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({"standard": "nymira", "version": "1.0.0", "event": event, "data": data})
    ));
}
