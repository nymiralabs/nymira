//! The tripwire: what the vault must look like, compiled into the binary, against what it
//! looks like now. Any difference stops deposits and new leases.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mode::Mode;
use crate::rpc::{Rpc, RpcError};

/// `config()` fields that are pinned. The TVL cap and current TVL move with normal use and are
/// left out.
pub const PINNED_FIELDS: [&str; 16] = [
    "operator_pk",
    "clearance_pk",
    "operator_account",
    "token",
    "treasury",
    "owner",
    "chain_id",
    "denominations",
    "note_fee",
    "close_fee",
    "exit_delay_secs",
    "note_ttl",
    "bound_classes",
    "charge_step",
    "min_charge",
    "slot_rate",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    pub vault: String,
    pub code_hash: String,
    pub vk_hashes: Value,
    /// The pinned `config()` fields, by name.
    pub config: Value,
    /// `{public_key, permission}` of every access key, sorted by key.
    pub access_keys: Vec<Value>,
}

/// The pins compiled in for a mode, `None` until a vault is pinned for it.
pub fn compiled(mode: Mode) -> Option<Pins> {
    let text = match mode {
        Mode::Testnet => include_str!("../pins/testnet.json"),
        Mode::Mainnet => include_str!("../pins/mainnet.json"),
    };
    serde_json::from_str(text).ok()
}

/// Demo builds only: the pins of the vault the demo deployed, written next to the state.
#[cfg(feature = "mock")]
pub fn demo(home: &std::path::Path) -> Option<Pins> {
    serde_json::from_slice(&std::fs::read(home.join("demo-pins.json")).ok()?).ok()
}

/// What the vault shows now.
pub async fn read_live(rpc: &Rpc, vault: &str) -> Result<Pins, RpcError> {
    let config = rpc.view(vault, "config", json!({})).await?;
    let mut pinned = serde_json::Map::new();
    for field in PINNED_FIELDS {
        pinned.insert(field.into(), config[field].clone());
    }
    Ok(Pins {
        vault: vault.into(),
        code_hash: rpc.code_hash(vault).await?,
        vk_hashes: rpc.view(vault, "vk_hashes", json!({})).await?,
        config: Value::Object(pinned),
        access_keys: rpc.access_keys(vault).await?,
    })
}

/// The text of a pin file (`pins/<network>.json`).
pub fn render(pins: &Pins) -> String {
    serde_json::to_string_pretty(pins).expect("pins serialize")
}

/// The differences, by name; empty when the vault matches.
pub fn diff(pinned: &Pins, live: &Pins) -> Vec<String> {
    let mut out = Vec::new();
    if pinned.vault != live.vault {
        out.push("vault account".into());
    }
    if pinned.code_hash != live.code_hash {
        out.push("code hash".into());
    }
    if pinned.vk_hashes != live.vk_hashes {
        out.push("verifying key hashes".into());
    }
    for field in PINNED_FIELDS {
        if pinned.config[field] != live.config[field] {
            out.push(format!("config.{field}"));
        }
    }
    if pinned.access_keys != live.access_keys {
        out.push("access keys".into());
    }
    out
}

/// The lines shown for a failed check. A change in the lease parameters is what the vault's
/// `params_applied` leaves behind, so it also says what to do about it.
pub fn warnings(diffs: &[String]) -> Vec<String> {
    let mut out = vec![format!(
        "the vault differs from the pins in this build: {}. Deposits are refused.",
        diffs.join(", ")
    )];
    const PARAMS: [&str; 4] = [
        "config.bound_classes",
        "config.charge_step",
        "config.min_charge",
        "config.slot_rate",
    ];
    if diffs.iter().any(|d| PARAMS.contains(&d.as_str())) {
        out.push(
            "The owner applied new lease parameters (params_applied). Update nymirad before opening a new lease; closing and exiting keep working."
                .into(),
        );
    }
    out
}

/// Holds the result of the last check.
#[derive(Default)]
pub struct Tripwire {
    tripped: Mutex<Option<Vec<String>>>,
}

impl Tripwire {
    /// Compares and remembers. `Ok` only when nothing differs.
    pub async fn check(&self, mode: Mode, rpc: &Rpc, vault: &str) -> Result<(), Vec<String>> {
        let result = match compiled(mode) {
            None => Err(vec![format!(
                "no vault is pinned for {mode:?} in this build"
            )]),
            Some(pinned) => match read_live(rpc, vault).await {
                Ok(live) => match diff(&pinned, &live) {
                    d if d.is_empty() => Ok(()),
                    d => Err(d),
                },
                Err(e) => Err(vec![format!("the vault could not be read: {e}")]),
            },
        };
        self.record(result)
    }

    /// The same with explicit pins (tests, and builds that carry their own).
    pub async fn check_against(&self, pinned: &Pins, rpc: &Rpc) -> Result<(), Vec<String>> {
        let result = match read_live(rpc, &pinned.vault).await {
            Ok(live) => match diff(pinned, &live) {
                d if d.is_empty() => Ok(()),
                d => Err(d),
            },
            Err(e) => Err(vec![format!("the vault could not be read: {e}")]),
        };
        self.record(result)
    }

    fn record(&self, result: Result<(), Vec<String>>) -> Result<(), Vec<String>> {
        *self.tripped.lock().unwrap_or_else(|e| e.into_inner()) = result.clone().err();
        result
    }

    /// The differences found by the last check, if any.
    pub fn tripped(&self) -> Option<Vec<String>> {
        self.tripped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::warnings;

    #[test]
    fn a_parameter_change_says_to_update_and_other_changes_do_not() {
        let owner = warnings(&["config.owner".into()]);
        assert_eq!(owner.len(), 1);
        assert!(owner[0].contains("config.owner"));
        let params = warnings(&["code hash".into(), "config.slot_rate".into()]);
        assert_eq!(params.len(), 2);
        assert!(params[1].contains("params_applied"));
    }
}
