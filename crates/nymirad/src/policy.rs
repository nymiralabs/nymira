//! The owner's signed policy for one agent.
//!
//! The owner writes `policy.yaml`, signs it with a full-access key of their NEAR account
//! (NEP-413) and hands `nymirad` the signed file. `nymirad` checks the signature and the key,
//! shows a summary, and only after the owner confirms does the policy take effect. A policy
//! that is unsigned, altered, replayed or meant for another agent counts as no policy.
//!
//! The signed file is JSON: `account_id`, `public_key` (`ed25519:<base58>`), `signature`,
//! `nonce` (32 bytes) and `message` (strings, bytes in base64), and `recipient`. The message is
//! itself JSON, `{"kind":"nymira-policy","version":N,"policy":"<the yaml text>"}`, so the
//! signature covers the exact policy bytes. The recipient is `nymirad:<agent>:<note commitment>`.
//!
//! The YAML is a small subset parsed by hand, and only after the signature has verified: one
//! `key: value` per line, block or flow lists of plain words, `#` comment lines. Anchors, tags,
//! merge keys, quotes, nested maps and multiple documents do not exist in it.

use std::collections::HashSet;
use std::path::Path;

use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::agents::{AgentEntry, Registry};
use crate::rpc::Rpc;

pub const KIND: &str = "nymira-policy";
/// NEP-413 prefix: 2^31 + 413.
const NEP413_TAG: u32 = 2_147_484_061;
const LIMIT_KEYS: [&str; 5] = [
    "max_lease_usd",
    "max_call_usd",
    "max_daily_usd",
    "confirm_above_usd",
    "rate_limit_per_min",
];

/// The file in the state directory that holds an agent's signed policy.
pub fn file_name(agent: &str) -> String {
    format!("policy-{agent}.json")
}

/// The recipient tag a policy for this agent and note must be signed for.
pub fn recipient(agent: &str, note: &str) -> String {
    format!("nymirad:{agent}:{note}")
}

/// What a policy is checked against: the pinned models and the smallest bound class.
#[derive(Clone, Debug)]
pub struct Context {
    pub models: Vec<String>,
    pub smallest_bound_usd: f64,
}

impl Context {
    /// `bound_classes` are micro-USD. With none, no `max_lease_usd` is large enough.
    pub fn new(models: Vec<String>, bound_classes_micro: &[u128]) -> Self {
        Self {
            models,
            smallest_bound_usd: bound_classes_micro
                .iter()
                .min()
                .map_or(f64::INFINITY, |m| *m as f64 / 1e6),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub allowed_models: Vec<String>,
    pub max_lease_usd: f64,
    pub max_call_usd: f64,
    pub max_daily_usd: f64,
    pub confirm_above_usd: f64,
    pub rate_limit_per_min: u32,
    pub require_tee: bool,
}

fn word(s: &str) -> Result<&str, String> {
    let ok = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'));
    if ok {
        Ok(s)
    } else {
        Err(format!(
            "`{s}` is not a plain word (letters, digits and . _ / - only; no quotes, anchors, tags or inline comments)"
        ))
    }
}

fn amount(key: &str, s: &str) -> Result<f64, String> {
    let plain = s.len() <= 12
        && s.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && s.matches('.').count() <= 1
        && !s.ends_with('.');
    let v: f64 = s
        .parse()
        .map_err(|_| format!("{key}: `{s}` is not a number"))?;
    if plain && v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!(
            "{key}: must be a finite positive number like 0.25, got `{s}`"
        ))
    }
}

/// Parses the restricted YAML. Call it only on bytes whose signature has verified.
pub fn parse(text: &str, ctx: &Context) -> Result<Policy, String> {
    const ALL: [&str; 8] = [
        "allowed_models",
        "max_lease_usd",
        "max_call_usd",
        "max_daily_usd",
        "confirm_above_usd",
        "rate_limit_per_min",
        "require_tee",
        "allowed_origins",
    ];
    let mut seen = HashSet::new();
    let mut scalars: Vec<(&str, &str)> = Vec::new();
    let mut lists: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut open: Option<usize> = None;
    for raw in text.lines() {
        if raw.contains('\t') {
            return Err("tabs are not allowed".into());
        }
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(item) = line
            .strip_prefix("  - ")
            .or_else(|| line.strip_prefix("- "))
        {
            let at = open.ok_or("a list item with no list key above it")?;
            lists[at].1.push(word(item.trim())?);
            continue;
        }
        open = None;
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("`{line}` is not `key: value`"))?;
        if !ALL.contains(&key) {
            return Err(format!("unknown key `{key}`"));
        }
        if !seen.insert(key) {
            return Err(format!("repeated key `{key}`"));
        }
        let value = value.trim();
        let is_list = matches!(key, "allowed_models" | "allowed_origins");
        match (is_list, value) {
            (true, "") => {
                lists.push((key, Vec::new()));
                open = Some(lists.len() - 1);
            }
            (true, v) if v.starts_with('[') && v.ends_with(']') => {
                let inner = v[1..v.len() - 1].trim();
                let items = if inner.is_empty() {
                    Vec::new()
                } else {
                    inner
                        .split(',')
                        .map(|i| word(i.trim()))
                        .collect::<Result<_, _>>()?
                };
                lists.push((key, items));
            }
            (true, _) => return Err(format!("{key}: must be a list")),
            (false, "") => return Err(format!("{key}: no value")),
            (false, v) => scalars.push((key, word(v)?)),
        }
    }
    let scalar = |k: &str| scalars.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    let list = |k: &str| lists.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    for k in LIMIT_KEYS.iter().chain(["allowed_models"].iter()) {
        if !seen.contains(k) {
            return Err(format!("missing key `{k}`"));
        }
    }
    let usd = |k: &str| amount(k, scalar(k).expect("checked above"));
    let max_lease_usd = usd("max_lease_usd")?;
    if max_lease_usd < ctx.smallest_bound_usd {
        return Err(format!(
            "max_lease_usd: {max_lease_usd} is below the smallest bound class ({} USD)",
            ctx.smallest_bound_usd
        ));
    }
    let rate = scalar("rate_limit_per_min").expect("checked above");
    let rate_limit_per_min = rate
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| rate.parse::<u32>().ok())
        .flatten()
        .filter(|r| *r > 0)
        .ok_or("rate_limit_per_min: must be a whole number above 0")?;
    let require_tee = match scalar("require_tee") {
        None | Some("true") => true,
        Some("false") => false,
        Some(v) => return Err(format!("require_tee: `{v}` is neither true nor false")),
    };
    if list("allowed_origins").is_some_and(|o| !o.is_empty()) {
        return Err("allowed_origins must be empty until paying sellers ships".into());
    }
    let allowed_models: Vec<String> = list("allowed_models")
        .expect("checked above")
        .into_iter()
        .map(String::from)
        .collect();
    if allowed_models.is_empty() {
        return Err("allowed_models: must not be empty".into());
    }
    for (i, m) in allowed_models.iter().enumerate() {
        if !ctx.models.contains(m) {
            return Err(format!("allowed_models: `{m}` is not a pinned model"));
        }
        if allowed_models[..i].contains(m) {
            return Err(format!("allowed_models: `{m}` is listed twice"));
        }
    }
    Ok(Policy {
        allowed_models,
        max_lease_usd,
        max_call_usd: usd("max_call_usd")?,
        max_daily_usd: usd("max_daily_usd")?,
        confirm_above_usd: usd("confirm_above_usd")?,
        rate_limit_per_min,
        require_tee,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    account_id: String,
    public_key: String,
    signature: String,
    nonce: String,
    message: String,
    recipient: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    kind: String,
    version: u32,
    policy: String,
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| "bad base64".to_string())
}

fn borsh_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// SHA-256 of the NEP-413 preimage: the tag, then the borsh payload (callback URL none).
pub fn nep413_hash(message: &str, nonce: &[u8; 32], recipient: &str) -> [u8; 32] {
    let mut pre = NEP413_TAG.to_le_bytes().to_vec();
    borsh_str(&mut pre, message);
    pre.extend(nonce);
    borsh_str(&mut pre, recipient);
    pre.push(0);
    Sha256::digest(pre).into()
}

/// A policy whose signature, account, recipient and content have all checked out.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub version: u32,
    pub policy: Policy,
    pub public_key: String,
}

/// Everything that needs no network: the account is the agent's owner, the recipient names this
/// agent and note, the signature is valid over the exact message, and only then the content.
pub fn check_signed(bytes: &[u8], agent: &AgentEntry, ctx: &Context) -> Result<Loaded, String> {
    let env: Envelope = serde_json::from_slice(bytes).map_err(|e| format!("signed policy: {e}"))?;
    if env.account_id != agent.owner_account {
        return Err(format!(
            "signed by {}, not by the budget note's owner account {}",
            env.account_id, agent.owner_account
        ));
    }
    if env.recipient != recipient(&agent.name, &agent.note) {
        return Err("signed for another agent or note".into());
    }
    let key = env
        .public_key
        .strip_prefix("ed25519:")
        .and_then(|k| bs58::decode(k).into_vec().ok())
        .and_then(|k| <[u8; 32]>::try_from(k).ok())
        .and_then(|k| VerifyingKey::from_bytes(&k).ok())
        .ok_or("the public key is not an ed25519 key")?;
    let nonce: [u8; 32] = b64(&env.nonce)?
        .try_into()
        .map_err(|_| "the nonce must be 32 bytes")?;
    let sig: [u8; 64] = b64(&env.signature)?
        .try_into()
        .map_err(|_| "the signature must be 64 bytes")?;
    key.verify_strict(
        &nep413_hash(&env.message, &nonce, &env.recipient),
        &Signature::from_bytes(&sig),
    )
    .map_err(|_| "the signature does not match the message")?;
    // Signature verified: now the content.
    let msg: Message = serde_json::from_str(&env.message).map_err(|e| format!("message: {e}"))?;
    if msg.kind != KIND || msg.version == 0 {
        return Err("the message is not a Nymira policy with a version of 1 or more".into());
    }
    Ok(Loaded {
        version: msg.version,
        policy: parse(&msg.policy, ctx)?,
        public_key: env.public_key,
    })
}

/// The policy in force for `agent`: the stored file must still verify and carry the version
/// the owner confirmed. Anything else is no policy.
pub fn in_force(home: &Path, agent: &AgentEntry, ctx: &Context) -> Option<Policy> {
    let confirmed = agent.policy_version?;
    let bytes = std::fs::read(home.join(file_name(&agent.name))).ok()?;
    let loaded = check_signed(&bytes, agent, ctx).ok()?;
    (loaded.version == confirmed).then_some(loaded.policy)
}

/// The signing key must be a full-access key of `account` on every endpoint, read at a final
/// block. One endpoint that disagrees or fails refuses the policy.
pub async fn check_key(rpcs: &[Rpc], account: &str, public_key: &str) -> Result<(), String> {
    if rpcs.is_empty() {
        return Err("no RPC endpoint to read the access keys from".into());
    }
    for rpc in rpcs {
        let keys = rpc
            .access_keys(account)
            .await
            .map_err(|e| format!("access keys of {account}: {e}"))?;
        let found = keys.iter().find(|k| k["public_key"] == public_key);
        match found.map(|k| &k["permission"]) {
            Some(p) if p == "FullAccess" => {}
            Some(_) => {
                return Err(format!(
                    "{public_key} is a function-call key, not a full-access key"
                ))
            }
            None => return Err(format!("{public_key} is not an access key of {account}")),
        }
    }
    Ok(())
}

/// What the owner is asked to confirm.
pub fn summary(agent: &AgentEntry, l: &Loaded) -> String {
    let p = &l.policy;
    format!(
        "Policy v{} for agent {} (note {}), signed by {} with {}\n  models: {}\n  per lease: {} USD, per call: {} USD, per day: {} USD\n  ask you above: {} USD per call\n  rate: {} requests a minute\n  verified attestation required: {}\n  sellers it may pay: none",
        l.version,
        agent.name,
        agent.note,
        agent.owner_account,
        l.public_key,
        p.allowed_models.join(", "),
        p.max_lease_usd,
        p.max_call_usd,
        p.max_daily_usd,
        p.confirm_above_usd,
        p.rate_limit_per_min,
        if p.require_tee { "yes" } else { "no" },
    )
}

/// The version a new policy must beat: the highest ever confirmed or restored.
pub fn floor(agent: &AgentEntry) -> u32 {
    agent.policy_seen.max(agent.policy_version.unwrap_or(0))
}

/// Takes effect: writes the file (0600, in the state directory) and records the version. The
/// agent's OS user cannot write there; a state directory open to group or others is refused.
pub fn activate(
    registry: &Registry,
    name: &str,
    bytes: &[u8],
    ctx: &Context,
) -> Result<Loaded, String> {
    use std::os::unix::fs::PermissionsExt;
    let home = registry.home().ok_or("no state directory here")?;
    let mode = std::fs::metadata(home)
        .map_err(|e| format!("state directory: {e}"))?
        .permissions()
        .mode();
    if mode & 0o022 != 0 {
        return Err("the state directory is writable by others; fix it (chmod 700) first".into());
    }
    let book = registry.load()?;
    let agent = book
        .agents
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no agent named {name}"))?;
    let loaded = check_signed(bytes, agent, ctx)?;
    if loaded.version <= floor(agent) {
        return Err(format!(
            "version {} does not rise: the last one seen is {}",
            loaded.version,
            floor(agent)
        ));
    }
    let path = home.join(file_name(name));
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp).map_err(|e| e.to_string())?, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", file_name(name)))?;
    registry.update(|b| {
        let a = b
            .agents
            .iter_mut()
            .find(|a| a.name == name)
            .ok_or("agent vanished")?;
        a.policy_version = Some(loaded.version);
        a.policy_seen = a.policy_seen.max(loaded.version);
        Ok(())
    })?;
    Ok(loaded)
}

/// The whole load: signature and content, the key on chain, the owner's confirmation, then the
/// policy takes effect on the agent's next request.
pub async fn load(
    registry: &Registry,
    name: &str,
    bytes: &[u8],
    ctx: &Context,
    rpcs: &[Rpc],
    confirm: &dyn Fn(&str) -> bool,
) -> Result<Loaded, String> {
    let book = registry.load()?;
    let agent = book
        .agents
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no agent named {name}"))?;
    let loaded = check_signed(bytes, agent, ctx)?;
    if loaded.version <= floor(agent) {
        return Err(format!(
            "version {} does not rise: the last one seen is {}",
            loaded.version,
            floor(agent)
        ));
    }
    check_key(rpcs, &agent.owner_account, &loaded.public_key).await?;
    if !confirm(&summary(agent, &loaded)) {
        return Err("not confirmed; the policy was not loaded".into());
    }
    activate(registry, name, bytes, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::new(vec!["m/one".into(), "m/two".into()], &[100_000, 500_000])
    }

    const GOOD: &str = "allowed_models:\n  - m/one\nmax_lease_usd: 0.5\nmax_call_usd: 0.05\nmax_daily_usd: 2\nconfirm_above_usd: 0.03\nrate_limit_per_min: 30\n";

    #[test]
    fn a_complete_policy_parses_and_require_tee_defaults_to_true() {
        let p = parse(GOOD, &ctx()).unwrap();
        assert_eq!(p.allowed_models, vec!["m/one"]);
        assert!(p.require_tee && p.rate_limit_per_min == 30 && p.max_lease_usd == 0.5);
        let flow = GOOD
            .replace("  - m/one", "")
            .replace("allowed_models:", "allowed_models: [m/one, m/two]");
        assert_eq!(parse(&flow, &ctx()).unwrap().allowed_models.len(), 2);
        let off = format!("{GOOD}require_tee: false\nallowed_origins: []\n");
        assert!(!parse(&off, &ctx()).unwrap().require_tee);
    }

    #[test]
    fn yaml_features_the_format_does_not_have_are_refused() {
        for bad in [
            "&a m/one",
            "*a",
            "!!str m/one",
            "\"m/one\"",
            "'m/one'",
            "m/one # note",
            "|",
            "{a: b}",
        ] {
            let text = GOOD.replace("m/one", bad);
            assert!(parse(&text, &ctx()).is_err(), "{bad}");
        }
        for extra in [
            "<<: x\n",
            "---\n",
            "&base x: 1\n",
            "base: &a 1\n",
            "\tmax_call_usd: 1\n",
        ] {
            assert!(parse(&format!("{GOOD}{extra}"), &ctx()).is_err(), "{extra}");
        }
    }

    #[test]
    fn limits_models_and_origins_are_checked() {
        let c = ctx();
        for (from, to) in [
            ("max_call_usd: 0.05\n", ""),
            ("max_call_usd: 0.05", "max_call_usd: 0"),
            ("max_call_usd: 0.05", "max_call_usd: -1"),
            ("max_call_usd: 0.05", "max_call_usd: 1e3"),
            ("max_call_usd: 0.05", "max_call_usd: nan"),
            ("max_call_usd: 0.05", "max_call_usd: inf"),
            ("max_lease_usd: 0.5", "max_lease_usd: 0.05"),
            ("rate_limit_per_min: 30", "rate_limit_per_min: 0"),
            ("rate_limit_per_min: 30", "rate_limit_per_min: 1.5"),
            ("  - m/one", "  - m/unpinned"),
            ("  - m/one", "  - m/one\n  - m/one"),
            ("  - m/one", ""),
            ("max_call_usd: 0.05", "max_call_usd: 0.05\nmax_call_usd: 9"),
        ] {
            assert!(
                parse(&GOOD.replace(from, to), &c).is_err(),
                "{from} -> {to}"
            );
        }
        assert!(parse(&format!("{GOOD}colour: red\n"), &c).is_err());
        assert!(parse(&format!("{GOOD}require_tee: maybe\n"), &c).is_err());
        assert!(parse(&format!("{GOOD}allowed_origins:\n  - shop.example\n"), &c).is_err());
        assert!(parse(&format!("{GOOD}allowed_origins: [shop.example]\n"), &c).is_err());
        assert!(parse(&format!("{GOOD}allowed_origins:\n"), &c).is_ok());
    }

    #[test]
    fn the_nep413_hash_changes_with_every_part() {
        let n = [7u8; 32];
        let base = nep413_hash("m", &n, "r");
        assert_ne!(base, nep413_hash("m2", &n, "r"));
        assert_ne!(base, nep413_hash("m", &[8u8; 32], "r"));
        assert_ne!(base, nep413_hash("m", &n, "r2"));
    }
}
