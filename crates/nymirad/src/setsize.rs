//! The anonymity set of a lease: how many notes could have paid for it, counted on this machine
//! from the vault's `note_records` at the final block the proof's tree was read at. The
//! operator's counts are never used. Its team list can only remove notes from the count.
//!
//! The rule (parent spec, "Set size rule"): note IDs below the leaf count `L` whose status is
//! `Active` and whose expiry is at or after the request time. Notes in a pending exit and closed
//! notes drop out. External N removes the union of the pinned and the operator's team list; the
//! total is the count before that.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rpc::{Rpc, RpcError};
use crate::strings;

/// The team's note IDs, from the file the repository publishes (the operator loads the same
/// file). Compiled into each release.
pub const TEAM_PIN: &str = include_str!("../../nymira-operator/team-note-ids.json");

/// The vault's page cap for `note_records`.
const PAGE: u32 = 100;

pub fn pinned_team() -> Vec<u32> {
    serde_json::from_str(TEAM_PIN).expect("team-note-ids.json is a list of note IDs")
}

#[derive(Debug, thiserror::Error)]
pub enum SetSizeError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the vault returned a malformed {0}")]
    Malformed(&'static str),
}

/// The note IDs one provider lists as able to pay, read at the block `at`.
async fn countable(
    rpc: &Rpc,
    vault: &str,
    at: &Value,
    leaf_count: u32,
    request_time: u64,
) -> Result<BTreeSet<u32>, SetSizeError> {
    let mut out = BTreeSet::new();
    let mut from = 0u32;
    while from < leaf_count {
        let limit = PAGE.min(leaf_count - from);
        let page = rpc
            .view_at(
                at.clone(),
                vault,
                "note_records",
                json!({ "from": from, "limit": limit }),
            )
            .await?;
        let page = page
            .as_array()
            .filter(|p| !p.is_empty() && p.len() <= limit as usize)
            .ok_or(SetSizeError::Malformed("note_records"))?;
        for (i, record) in page.iter().enumerate() {
            let id = record["note_id"].as_u64();
            if id != Some(u64::from(from) + i as u64) {
                return Err(SetSizeError::Malformed("note_records"));
            }
            let expiry = record["expiry"]
                .as_u64()
                .ok_or(SetSizeError::Malformed("note_records"))?;
            if record["status"] == "Active" && expiry >= request_time {
                out.insert(from + i as u32);
            }
        }
        from += page.len() as u32;
    }
    Ok(out)
}

/// What the providers listed, before the team list is taken out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raw {
    /// One set per provider that answered; the first is always there.
    pub sets: Vec<BTreeSet<u32>>,
    /// Why there is no second set, if there is none.
    pub one_provider: Option<String>,
}

/// Reads the sets at `block_hash`: from `first` (an error fails the read) and from `second`
/// (an error is kept as the reason there is only one set).
pub async fn read(
    first: &Rpc,
    second: Option<&Rpc>,
    vault: &str,
    block_hash: &str,
    leaf_count: u32,
    request_time: u64,
) -> Result<Raw, SetSizeError> {
    let at = json!({ "block_id": block_hash });
    let mut sets = vec![countable(first, vault, &at, leaf_count, request_time).await?];
    let one_provider = match second {
        None => Some("no second provider is configured".to_string()),
        Some(rpc) => match countable(rpc, vault, &at, leaf_count, request_time).await {
            Ok(set) => {
                sets.push(set);
                None
            }
            Err(e) => Some(e.to_string()),
        },
    };
    Ok(Raw { sets, one_provider })
}

/// The same rule at the current final block of `first`, at `request_time`.
pub async fn read_now(
    first: &Rpc,
    second: Option<&Rpc>,
    vault: &str,
    request_time: u64,
) -> Result<Raw, SetSizeError> {
    let (hash, _) = first.final_block().await?;
    let leaf_count = first
        .view_at(json!({ "block_id": hash }), vault, "leaf_count", json!({}))
        .await?
        .as_u64()
        .ok_or(SetSizeError::Malformed("leaf_count"))? as u32;
    read(first, second, vault, &hash, leaf_count, request_time).await
}

/// The figure kept with a lease record. Old records have none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSize {
    /// Notes that could have paid, minus the union team list. The lower provider's figure.
    pub external: u32,
    /// The same count before the team list is taken out.
    pub total: u32,
    /// Team IDs only the pinned list has, and only the operator's list has.
    #[serde(default)]
    pub pinned_only: Vec<u32>,
    #[serde(default)]
    pub operator_only: Vec<u32>,
    /// The operator's `/v1/status` gave no list.
    #[serde(default)]
    pub operator_list_missing: bool,
    /// (external, total) of each provider, when the two differ.
    #[serde(default)]
    pub providers_differ: Option<[(u32, u32); 2]>,
    /// Why only one provider was read, if so.
    #[serde(default)]
    pub one_provider: Option<String>,
}

/// Applies the team lists to what the providers listed. The operator's list can only shrink the
/// count: a pinned ID it leaves out stays removed.
pub fn evaluate(raw: &Raw, pinned: &[u32], operator: Option<&[u32]>) -> SetSize {
    let pinned: BTreeSet<u32> = pinned.iter().copied().collect();
    let operator_set: Option<BTreeSet<u32>> = operator.map(|o| o.iter().copied().collect());
    let team: BTreeSet<u32> = match &operator_set {
        Some(o) => pinned.union(o).copied().collect(),
        None => pinned.clone(),
    };
    let figures: Vec<(u32, u32)> = raw
        .sets
        .iter()
        .map(|s| (s.difference(&team).count() as u32, s.len() as u32))
        .collect();
    let (external, total) = *figures.iter().min().expect("one provider answered");
    SetSize {
        external,
        total,
        pinned_only: operator_set
            .as_ref()
            .map_or_else(Vec::new, |o| pinned.difference(o).copied().collect()),
        operator_only: operator_set
            .as_ref()
            .map_or_else(Vec::new, |o| o.difference(&pinned).copied().collect()),
        operator_list_missing: operator_set.is_none(),
        providers_differ: (raw.sets.len() == 2 && raw.sets[0] != raw.sets[1])
            .then(|| [figures[0], figures[1]]),
        one_provider: raw.one_provider.clone(),
    }
}

/// The team IDs in `/v1/status`, if it lists any (an empty list is a list).
pub fn operator_team(status: &Value) -> Option<Vec<u32>> {
    status["team_note_ids"]
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()))
        .collect()
}

impl SetSize {
    /// "set size: N (external), total T".
    pub fn headline(&self) -> String {
        format!(
            "{}, {}",
            strings::fill("set_size", &[("n", self.external.to_string())]),
            strings::fill("set_total", &[("n", self.total.to_string())])
        )
    }

    /// What differs between the lists and the providers, one sentence each.
    pub fn differences(&self) -> Vec<String> {
        let ids = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let mut out = Vec::new();
        if !self.pinned_only.is_empty() {
            out.push(strings::fill(
                "team_pinned_only",
                &[("ids", ids(&self.pinned_only))],
            ));
        }
        if !self.operator_only.is_empty() {
            out.push(strings::fill(
                "team_operator_only",
                &[("ids", ids(&self.operator_only))],
            ));
        }
        if self.operator_list_missing {
            out.push(strings::get("operator_list_missing").to_string());
        }
        if let Some([a, b]) = self.providers_differ {
            out.push(strings::fill(
                "providers_differ",
                &[("a", a.0.to_string()), ("b", b.0.to_string())],
            ));
        }
        if let Some(why) = &self.one_provider {
            out.push(strings::fill("one_provider", &[("why", why.clone())]));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(sets: &[&[u32]]) -> Raw {
        Raw {
            sets: sets.iter().map(|s| s.iter().copied().collect()).collect(),
            one_provider: None,
        }
    }

    #[test]
    fn the_published_team_list_parses() {
        let _ = pinned_team();
    }

    #[test]
    fn the_team_is_taken_out_of_the_external_count_only() {
        let r = raw(&[&[0, 1, 2, 3, 4, 5]]);
        let s = evaluate(&r, &[0, 1, 2], Some(&[0, 1, 2]));
        assert_eq!((s.external, s.total), (3, 6));
        assert_eq!(s.headline(), "set size: 3 (external), total 6");
        assert!(s.differences().is_empty());
        // A team ID that is not in the set (closed, or at or above L) changes nothing.
        let s = evaluate(&r, &[0, 1, 2, 40], None);
        assert_eq!((s.external, s.total), (3, 6));
    }

    #[test]
    fn the_operator_can_only_lower_the_count() {
        let r = raw(&[&[0, 1, 2, 3, 4, 5]]);
        // It leaves pinned ID 2 out: the pin still removes it, and the gap is shown.
        let left_out = evaluate(&r, &[0, 1, 2], Some(&[0, 1]));
        assert_eq!(left_out.external, 3);
        assert_eq!(left_out.pinned_only, [2]);
        assert!(left_out.differences()[0].contains("only in this release's pinned list: 2"));
        // It adds ID 3: N falls.
        let added = evaluate(&r, &[0, 1, 2], Some(&[0, 1, 2, 3]));
        assert_eq!((added.external, added.total), (2, 6));
        assert_eq!(added.operator_only, [3]);
        // No list at all: the pin alone counts, and that is said.
        let silent = evaluate(&r, &[0, 1, 2], None);
        assert_eq!(silent.external, 3);
        assert!(silent.operator_list_missing);
    }

    #[test]
    fn the_lower_provider_wins_and_the_difference_is_shown() {
        let r = raw(&[&[0, 1, 2, 3, 4], &[0, 1, 2, 3]]);
        let s = evaluate(&r, &[0], Some(&[0]));
        assert_eq!((s.external, s.total), (3, 4));
        assert_eq!(s.providers_differ, Some([(4, 5), (3, 4)]));
        assert!(s.differences()[0].contains("set size 4 and 3"));
        let same = evaluate(&raw(&[&[0, 1], &[0, 1]]), &[], Some(&[]));
        assert!(same.providers_differ.is_none());
    }

    #[test]
    fn operator_team_reads_the_status_list() {
        assert_eq!(
            operator_team(&json!({"team_note_ids": [1, 2]})),
            Some(vec![1, 2])
        );
        assert_eq!(operator_team(&json!({"team_note_ids": []})), Some(vec![]));
        assert_eq!(operator_team(&json!({})), None);
        assert_eq!(operator_team(&json!({"team_note_ids": ["x"]})), None);
    }
}
