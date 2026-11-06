//! The privacy sentences. They live in `shared/privacy-strings.json`, one flat map from key to
//! text, so `nymirad status` and the web panel say the same thing. The file is compiled in.

use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const RAW: &str = include_str!("../../../shared/privacy-strings.json");

fn table() -> &'static BTreeMap<String, String> {
    static TABLE: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        serde_json::from_str(RAW).expect("shared/privacy-strings.json is a flat map of strings")
    })
}

/// The sentence for `key`. A missing key is a build bug, caught by the tests.
pub fn get(key: &str) -> &'static str {
    table()
        .get(key)
        .unwrap_or_else(|| panic!("shared/privacy-strings.json has no key `{key}`"))
}

/// The sentence for `key` with each `{name}` replaced.
pub fn fill(key: &str, vars: &[(&str, String)]) -> String {
    let mut text = get(key).to_string();
    for (name, value) in vars {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn the_shared_file_is_pinned() {
        // The web panel reads the same file. Changing a sentence means changing this hash on
        // purpose, after checking the new text against THREAT_MODEL.md.
        assert_eq!(
            hex::encode(Sha256::digest(RAW.as_bytes())),
            "4c7655b35a014c9d26a67086a7a7f881ceae5fac99ba1bec8ae70230f667a855"
        );
        let keys: Vec<&str> = table().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "balances_private",
                "five_not_safe",
                "includes_unlisted",
                "leave_answer",
                "leave_clearance",
                "leave_close",
                "leave_exit",
                "leave_exit_purpose",
                "leave_sender",
                "link_left",
                "link_open",
                "now_label",
                "one_provider",
                "operator_list_missing",
                "providers_differ",
                "reuse_accepted",
                "reuse_clearance_reserved",
                "reuse_evidence",
                "reuse_finalized",
                "reuse_left",
                "reuse_no_request",
                "reuse_none",
                "reuse_other",
                "reuse_read_reason",
                "reuse_refuses",
                "reuse_resent",
                "reuse_reserved",
                "reuse_rule",
                "reuse_says",
                "reuse_too_old",
                "reuse_unknown",
                "reuse_unreachable",
                "set_size",
                "set_total",
                "team_operator_only",
                "team_pinned_only",
                "testnet_notes",
                "tor_limits",
                "tor_off",
                "tor_on",
                "unavailable",
                "who_attest",
                "who_certs",
                "who_chain",
                "who_entry",
                "who_host",
                "who_intents",
                "who_model",
                "who_near_ai",
                "who_operator",
                "who_rpc",
                "who_second",
            ]
        );
    }

    #[test]
    fn placeholders_are_filled() {
        assert_eq!(
            fill("set_size", &[("n", "3".into())]),
            "set size: 3 (external)"
        );
        let both = fill("providers_differ", &[("a", "2".into()), ("b", "3".into())]);
        assert!(!both.contains('{'), "{both}");
    }

    #[test]
    fn no_sentence_has_an_em_dash() {
        assert!(table().values().all(|v| !v.contains('\u{2014}')));
    }
}
