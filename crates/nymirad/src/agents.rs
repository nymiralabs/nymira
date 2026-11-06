//! Agents and their budget notes: the registry in the owner's state directory, the `nyma_`
//! tokens (stored only as hashes), and the scope that keeps each token on its own note.
//!
//! The registry is one file, `agents.json`, written 0600 by the owner's CLI and read by `serve`
//! on every request, so a disabled token stops at once. The owner's own requests never select a
//! budget note: their scope excludes every note listed here.

use std::ffi::CString;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const FILE: &str = "agents.json";
pub const PREFIX: &str = "nyma_";

/// What the agent token may do over HTTP, on both listeners.
pub const AGENT_ROUTES: [(&str, &str); 2] =
    [("POST", "/v1/chat/completions"), ("GET", "/v1/models")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// The agent Unix socket, with the peer's user ID checked.
    Unix,
    /// Loopback TCP on 8788, directly or through an SSH forward.
    Tcp,
}

impl Transport {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "unix" => Some(Self::Unix),
            "tcp" => Some(Self::Tcp),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    /// SHA-256 of the whole token, hex. The token itself is never stored here.
    pub token_hash: String,
    /// The budget note's commitment.
    pub note: String,
    pub owner_account: String,
    /// The version of the signed policy in force; none until one is loaded.
    #[serde(default)]
    pub policy_version: Option<u32>,
    /// The highest policy version ever confirmed or restored. A new policy must beat it, so an
    /// older signed policy cannot be replayed.
    #[serde(default)]
    pub policy_seen: u32,
    pub transport: Transport,
    /// The agent's OS user, for a `unix` token.
    #[serde(default)]
    pub uid: Option<u32>,
    #[serde(default)]
    pub disabled: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    /// The group that may open the agent socket.
    #[serde(default)]
    pub socket_group: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentEntry>,
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{PREFIX}{}", hex::encode(bytes))
}

impl Book {
    /// The agent that owns `token`. Every entry is compared, so timing does not show which.
    pub fn find(&self, token: &str) -> Option<&AgentEntry> {
        let presented = hash_token(token);
        let mut found = None;
        for a in &self.agents {
            if bool::from(a.token_hash.as_bytes().ct_eq(presented.as_bytes())) {
                found = Some(a);
            }
        }
        found
    }

    /// Commitments of every budget note.
    pub fn budget_notes(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.note.clone()).collect()
    }

    pub fn for_note(&self, commitment: &str) -> Option<&AgentEntry> {
        self.agents.iter().find(|a| a.note == commitment)
    }
}

/// Which notes a request may draw on.
#[derive(Clone, Debug)]
pub enum Scope {
    /// The owner: every note except the budget notes.
    Owner { budget: Vec<String> },
    /// An agent: its own budget note and nothing else, under the limits of its signed policy.
    Agent {
        name: String,
        note: String,
        policy: crate::policy::Policy,
    },
}

impl Default for Scope {
    fn default() -> Self {
        Self::Owner { budget: Vec::new() }
    }
}

impl Scope {
    pub fn allows(&self, commitment: &str) -> bool {
        match self {
            Self::Owner { budget } => !budget.iter().any(|b| b == commitment),
            Self::Agent { note, .. } => note == commitment,
        }
    }

    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }

    /// The lease size the request may ask for. An agent gets the large class only when it asks
    /// for it and its policy's `max_lease_usd` allows it; otherwise the default class.
    pub fn lease_request(&self, asked: Option<f64>) -> Option<f64> {
        match self {
            Self::Owner { .. } => asked,
            Self::Agent { policy, .. } => asked
                .filter(|a| *a >= 0.5 && policy.max_lease_usd >= 0.5)
                .map(|a| a.min(policy.max_lease_usd)),
        }
    }
}

/// The registry file. `none()` never holds an agent (tests and builds that serve no agents).
#[derive(Clone, Debug)]
pub struct Registry(Option<PathBuf>);

impl Registry {
    pub fn at(home: &Path) -> Self {
        Self(Some(home.join(FILE)))
    }

    pub fn none() -> Self {
        Self(None)
    }

    /// The state directory the registry lives in.
    pub fn home(&self) -> Option<&Path> {
        self.0.as_deref().and_then(Path::parent)
    }

    /// A missing file is an empty book; one that cannot be read or parsed is an error, so a
    /// caller fails closed instead of treating a budget note as an ordinary one.
    pub fn load(&self) -> Result<Book, String> {
        let Some(path) = &self.0 else {
            return Ok(Book::default());
        };
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{FILE}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(e) => Err(format!("{FILE}: {e}")),
        }
    }

    /// Loads the book, lets `change` edit it, and writes it back (temp file, then rename).
    /// shortcut: no lock between two owner commands run at the same moment, add a flock if
    /// that ever happens.
    pub fn update<R>(
        &self,
        change: impl FnOnce(&mut Book) -> Result<R, String>,
    ) -> Result<R, String> {
        let path = self.0.as_ref().ok_or("no registry here")?;
        let mut book = self.load()?;
        let out = change(&mut book)?;
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).map_err(|e| format!("{FILE}: {e}"))?;
        file.write_all(&serde_json::to_vec_pretty(&book).expect("book serializes"))
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("{FILE}: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{FILE}: {e}"))?;
        Ok(out)
    }

    pub fn disable(&self, name: &str) -> Result<(), String> {
        self.update(|b| {
            let a = b
                .agents
                .iter_mut()
                .find(|a| a.name == name)
                .ok_or_else(|| format!("no agent named {name}"))?;
            a.disabled = true;
            Ok(())
        })
    }

    /// After a restore: keeps the higher of the backup's policy version and the one the owner
    /// last saw, so an older signed policy cannot be loaded again.
    pub fn raise_policy_seen(&self, name: &str, last_seen: u32) -> Result<(), String> {
        self.update(|b| {
            let a = b
                .agents
                .iter_mut()
                .find(|a| a.name == name)
                .ok_or_else(|| format!("no agent named {name}"))?;
            a.policy_seen = a.policy_seen.max(last_seen);
            Ok(())
        })
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// What `agent create` was asked for.
pub struct Spec {
    pub name: String,
    pub transport: Transport,
    pub uid: Option<u32>,
    pub group: Option<String>,
    pub gid: Option<u32>,
    pub token_file: PathBuf,
}

/// The result of registering: what to tell the owner.
pub struct Created {
    pub notes: Vec<String>,
}

impl Spec {
    /// Records the agent and writes its token file. Runs before the deposit transfer is signed,
    /// so the note is a budget note from the moment it exists.
    pub fn register(
        &self,
        registry: &Registry,
        commitment: &str,
        owner_account: &str,
        now_secs: u64,
    ) -> Result<Created, String> {
        let token = new_token();
        registry.update(|book| {
            if book.agents.iter().any(|a| a.name == self.name) {
                return Err(format!("an agent named {} already exists", self.name));
            }
            if self.transport == Transport::Unix {
                match (&book.socket_group, &self.group) {
                    (Some(old), Some(new)) if old != new => {
                        return Err(format!(
                            "the agent socket already belongs to group {old}; every socket agent shares one group"
                        ))
                    }
                    (None, Some(new)) => book.socket_group = Some(new.clone()),
                    _ => {}
                }
            }
            book.agents.push(AgentEntry {
                name: self.name.clone(),
                token_hash: hash_token(&token),
                note: commitment.to_string(),
                owner_account: owner_account.to_string(),
                policy_version: None,
                policy_seen: 0,
                transport: self.transport,
                uid: self.uid,
                disabled: false,
                created_at: now_secs,
            });
            Ok(())
        })?;
        let mut notes = Vec::new();
        let (mode, gid) = match self.transport {
            Transport::Unix => (0o640, self.gid),
            Transport::Tcp => (0o600, None),
        };
        let applied = write_token_file(&self.token_file, &token, mode, gid)
            .map_err(|e| format!("token file {}: {e}", self.token_file.display()))?;
        notes.push(format!("Token file: {}", self.token_file.display()));
        match (gid, applied) {
            (Some(_), false) => notes.push(format!(
                "You are not in group {}, so the file keeps your group and the agent cannot read it yet. Run: sudo chgrp {} {}",
                self.group.as_deref().unwrap_or("?"),
                self.group.as_deref().unwrap_or("?"),
                self.token_file.display()
            )),
            (Some(_), true) => notes.push(format!(
                "Mode 0640, group {}: the agent user can read it, no other user can.",
                self.group.as_deref().unwrap_or("?")
            )),
            _ => {}
        }
        Ok(Created { notes })
    }
}

/// Creates the token file (never overwrites one), writes the token, and sets the group if one
/// is given. Returns whether the group was applied; a failure there (the owner is not in the
/// group) is not an error, it is the `sudo` step the caller prints.
pub fn write_token_file(
    path: &Path,
    token: &str,
    mode: u32,
    gid: Option<u32>,
) -> std::io::Result<bool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    let mut file = options.open(path)?;
    // The umask may have cut the mode; set it exactly.
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    Ok(match gid {
        Some(g) => chown_group(path, g),
        None => true,
    })
}

/// Sets the group of `path`; false when the caller is not allowed to.
pub fn chown_group(path: &Path, gid: u32) -> bool {
    let Ok(c) = CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str())) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path; uid_t::MAX (-1) leaves the owner unchanged.
    unsafe { libc::chown(c.as_ptr(), libc::uid_t::MAX, gid) == 0 }
}

/// The user ID and primary group ID of a local user.
pub fn user_ids(name: &str) -> Option<(u32, u32)> {
    let c = CString::new(name).ok()?;
    // SAFETY: getpwnam returns null or a pointer valid until the next call; read at once.
    let p = unsafe { libc::getpwnam(c.as_ptr()) };
    (!p.is_null()).then(|| unsafe { ((*p).pw_uid, (*p).pw_gid) })
}

pub fn group_id(name: &str) -> Option<u32> {
    let c = CString::new(name).ok()?;
    // SAFETY: as in `user_ids`.
    let g = unsafe { libc::getgrnam(c.as_ptr()) };
    (!g.is_null()).then(|| unsafe { (*g).gr_gid })
}

pub fn group_name(gid: u32) -> Option<String> {
    // SAFETY: as in `user_ids`; the name is copied out before the next call.
    let g = unsafe { libc::getgrgid(gid) };
    (!g.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr((*g).gr_name) }
            .to_string_lossy()
            .into_owned()
    })
}

/// "1.234567 USDC" from base units.
fn usdc(base_units: u128) -> String {
    format!(
        "{}.{:06} USDC",
        base_units / 1_000_000,
        base_units % 1_000_000
    )
}

/// Shown when a budget note is created.
pub fn cap_hint(note_fee: u128) -> String {
    format!(
        "To cap a day's spend hard, deposit a fresh 1 USDC note each day. That costs a note fee every day ({}) and leaves a daily deposit pattern on your account. The daily limit in a signed policy is the softer way; it relies on this machine.",
        usdc(note_fee)
    )
}

/// What `agent create` tells the owner about the transport.
pub fn transport_note(t: Transport) -> &'static str {
    match t {
        Transport::Unix => "Transport unix: the token works on the agent socket only, for the agent's OS user. Restart `nymirad serve` once so the socket takes the group.",
        Transport::Tcp => "Transport tcp: the token works on loopback port 8788 only. A remote agent reached through an SSH forward needs a tcp token.",
    }
}

/// One line per agent, for `agent list`.
pub fn list_lines(book: &Book) -> Vec<String> {
    if book.agents.is_empty() {
        return vec!["No agents.".into()];
    }
    book.agents
        .iter()
        .map(|a| {
            format!(
                "{}: {:?} token, {}, policy {}, owner {}",
                a.name,
                a.transport,
                if a.disabled { "disabled" } else { "active" },
                a.policy_version
                    .map_or("none".to_string(), |v| format!("v{v}")),
                a.owner_account
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, token: &str, note: &str) -> AgentEntry {
        AgentEntry {
            name: name.into(),
            token_hash: hash_token(token),
            note: note.into(),
            owner_account: "owner.test".into(),
            policy_version: None,
            policy_seen: 0,
            transport: Transport::Tcp,
            uid: None,
            disabled: false,
            created_at: 1,
        }
    }

    #[test]
    fn a_token_finds_only_its_own_agent_and_the_book_holds_no_token() {
        let (a, b) = (new_token(), new_token());
        assert!(a.starts_with("nyma_") && a.len() == 5 + 64);
        let book = Book {
            socket_group: None,
            agents: vec![entry("a", &a, "0x01"), entry("b", &b, "0x02")],
        };
        assert_eq!(book.find(&a).unwrap().note, "0x01");
        assert_eq!(book.find(&b).unwrap().note, "0x02");
        assert!(book.find("nyma_nope").is_none());
        let text = serde_json::to_string(&book).unwrap();
        assert!(!text.contains(&a) && !text.contains(&b));
    }

    #[test]
    fn scopes_split_the_notes_between_the_owner_and_each_agent() {
        let owner = Scope::Owner {
            budget: vec!["0x01".into(), "0x02".into()],
        };
        assert!(owner.allows("0x99") && !owner.allows("0x01") && !owner.allows("0x02"));
        let agent = agent_scope(0.5);
        assert!(agent.allows("0x01") && !agent.allows("0x02") && !agent.allows("0x99"));
    }

    fn agent_scope(max_lease_usd: f64) -> Scope {
        Scope::Agent {
            name: "cursor".into(),
            note: "0x01".into(),
            policy: crate::policy::Policy {
                allowed_models: vec![],
                max_lease_usd,
                max_call_usd: 0.05,
                max_daily_usd: 2.0,
                confirm_above_usd: 0.03,
                rate_limit_per_min: 30,
                require_tee: true,
            },
        }
    }

    #[test]
    fn an_agent_gets_the_large_lease_only_when_it_asks_and_its_policy_allows() {
        let big = agent_scope(0.5);
        assert_eq!(big.lease_request(None), None);
        assert_eq!(big.lease_request(Some(0.2)), None);
        assert_eq!(big.lease_request(Some(5.0)), Some(0.5));
        let small = agent_scope(0.4);
        assert_eq!(small.lease_request(Some(5.0)), None);
        assert_eq!(small.lease_request(Some(0.5)), None);
        let owner = Scope::default();
        assert_eq!(owner.lease_request(Some(5.0)), Some(5.0));
    }

    #[test]
    fn the_hint_names_the_fee_and_the_deposit_pattern() {
        let hint = cap_hint(50_000);
        assert!(hint.contains("fresh 1 USDC note") && hint.contains("0.050000 USDC"));
        assert!(hint.contains("daily deposit pattern"));
    }

    #[test]
    fn names_are_short_and_plain() {
        assert!(valid_name("cursor-1") && valid_name("a_b"));
        assert!(
            !valid_name("")
                && !valid_name("Agent")
                && !valid_name("a b")
                && !valid_name(&"x".repeat(33))
        );
    }
}
