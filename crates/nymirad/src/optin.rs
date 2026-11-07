//! Opt-in mode: the budget note runs on the agent's own machine.
//!
//! The owner moves the note with `nymirad agent move`. That writes one file, the note's secret,
//! wallet and registry entry, encrypted to the agent machine's age key, for `nymirad restore`
//! there. From then on the owner's copy refuses to spend (`moved` in `agents.json`), and the
//! agent's `nymirad` writes each new state after a retire, encrypted to the owner's X25519 key
//! and numbered, into the places the owner chose. `agent import-state` picks the best of those
//! files for the owner.
//!
//! The move and the import are owner commands that read and write files. No route the agent
//! token can reach leads here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zkapi_client::note_state::NoteState;
use zkapi_proof::compact::verify_signature;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::Felt252;

use crate::agents::{Registry, Transport};
use crate::backup::{self, StateBackup, Target};
use crate::secrets::{NoteEntry, NoteStatus, NoteStore};
use crate::walletvault::{Files, WalletVault};

/// On the agent's machine: where and to whom each new state is written.
pub const FILE: &str = "optin.json";
const STATE_NAME: &str = "note_state.json";
const KIND: &str = "nymira-moved-state";
/// How far along a note's chain the import looks for a state before it gives up on it.
const MAX_CHAIN: u64 = 100_000;

/// Shown before the move; the owner must confirm it.
pub const WARNING: &str = "Moving this budget note to the agent's machine is an opt-in, and it changes what you can rely on: revocation is not guaranteed, the ceiling is. The most the agent can lose you is the note's balance, and the note can only be paid out to your own account. From now on the policy, the spending counters and the approvals run on the agent's machine and are advisory: anyone who controls that machine can change them. The note's secret goes with the move, so your own copy stops spending and the agent's machine can spend until the balance is gone.";

/// Shown with the warning.
pub const RACE: &str = "Revoking is then a race. Only what is unspent when your final close or exit lands goes to your payout account. A hostile agent can spend the rest on inference first. An exit from a stale state can be defeated by a newer one, so run `nymirad agent import-state` for the latest state before you close or exit.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Export {
    /// The owner's age X25519 public key.
    pub recipient: String,
    pub places: Vec<PathBuf>,
    /// The sequence number of the next file.
    pub next_seq: u64,
}

impl Export {
    pub fn load(home: &Path) -> Result<Option<Self>, String> {
        match std::fs::read(home.join(FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| format!("{FILE}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{FILE}: {e}")),
        }
    }

    fn save(&self, home: &Path) -> Result<(), String> {
        backup::write_private(
            &home.join(FILE),
            &serde_json::to_vec_pretty(self).expect("export serializes"),
        )
        .map_err(|e| format!("{FILE}: {e}"))
    }
}

/// What the owner chose for the move.
pub struct MoveRequest {
    pub agent: String,
    /// The age public key of the agent machine's `nymirad` backup.
    pub agent_recipient: String,
    /// The owner's age X25519 public key and the places the states go to.
    pub owner_recipient: String,
    pub places: Vec<PathBuf>,
    pub out: PathBuf,
}

/// Moves the budget note of `req.agent` out to `req.out` after the owner confirms. The owner's
/// copy is marked first, so a failure after that leaves a note nobody spends, never two copies.
pub fn move_note(
    home: &Path,
    passphrase: &str,
    rounds: u32,
    req: &MoveRequest,
    now_secs: u64,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    let agent_key = backup::parse_recipient(&req.agent_recipient)
        .map_err(|_| "--agent-recipient is not an age public key (age1...)".to_string())?;
    backup::parse_recipient(&req.owner_recipient)
        .map_err(|_| "--state-recipient is not an age public key (age1...)".to_string())?;
    if req.places.is_empty() {
        return Err("name at least one --state-place for the new states".into());
    }
    let registry = Registry::at(home);
    let book = registry.load()?;
    let agent = book
        .agents
        .iter()
        .find(|a| a.name == req.agent)
        .ok_or_else(|| format!("no agent named {}", req.agent))?
        .clone();
    if agent.moved {
        return Err(format!(
            "the note of {} is already on the agent's machine",
            req.agent
        ));
    }
    if agent.disabled {
        return Err(format!("agent {} is disabled", req.agent));
    }
    if agent.policy_version.is_none() {
        return Err("load a signed policy first (`agent policy`): without one the agent's machine refuses every request".into());
    }
    let notes = NoteStore::open(&home.join("notes.sealed"), passphrase, rounds)
        .map_err(|e| e.to_string())?;
    let entry = notes
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|n| n.commitment == agent.note)
        .ok_or("the budget note is not in this state directory")?;
    if entry.status != NoteStatus::Accepted {
        return Err(format!(
            "the note is {:?}; only an accepted note can move",
            entry.status
        ));
    }
    let vault = WalletVault::new(home, passphrase, rounds);
    let busy = || vault.work_dir(&entry.commitment).is_ok_and(|d| d.exists());
    if busy() {
        return Err("a lease is open on the note; let it settle and run the command again".into());
    }

    if !confirm(&format!(
        "{WARNING}\n\n{RACE}\n\nType yes to move the note of agent {}.",
        req.agent
    )) {
        return Err("not moved".into());
    }

    registry.update(|b| set_moved(b, &req.agent, true))?;
    let undo = |why: String| {
        let _ = registry.update(|b| set_moved(b, &req.agent, false));
        why
    };
    // A request that checked out the wallet before the mark landed.
    if busy() {
        return Err(undo(
            "a lease opened on the note while it moved; run the command again".into(),
        ));
    }
    let package = build_package(home, &vault, &entry, &agent, req, now_secs).map_err(&undo)?;
    let file = backup::encrypt(&Target::new(agent_key, vec![]), &package)
        .map_err(|e| undo(e.to_string()))?;
    backup::write_private(&req.out, &file)
        .map_err(|e| undo(format!("{}: {e}", req.out.display())))?;
    Ok(format!(
        "Moved. Your copy of the note of {} no longer spends. Copy {} to the agent's machine and run `nymirad restore --from` on it there, with the private key that goes with {}, into an empty state directory. The agent's nymirad writes each new state, encrypted to {} and numbered, to: {}. Run `nymirad agent import-state` with the private key whenever you need the latest one.",
        req.agent,
        req.out.display(),
        req.agent_recipient,
        req.owner_recipient,
        req.places
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

fn set_moved(book: &mut crate::agents::Book, name: &str, moved: bool) -> Result<(), String> {
    book.agents
        .iter_mut()
        .find(|a| a.name == name)
        .map(|a| a.moved = moved)
        .ok_or_else(|| format!("no agent named {name}"))
}

fn build_package(
    home: &Path,
    vault: &WalletVault,
    entry: &NoteEntry,
    agent: &crate::agents::AgentEntry,
    req: &MoveRequest,
    now_secs: u64,
) -> Result<Vec<u8>, String> {
    let wallets: BTreeMap<String, Files> = vault
        .export_all()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(k, _)| *k == entry.commitment)
        .collect();
    let mut state = StateBackup {
        version: backup::VERSION,
        taken_at: now_secs,
        notes: vec![entry.clone()],
        wallets,
        ..Default::default()
    };
    // The agent's registry holds this agent only. Its token is accepted on loopback TCP there:
    // a Unix user ID from this machine means nothing on that one.
    let mut theirs = agent.clone();
    theirs.moved = false;
    theirs.transport = Transport::Tcp;
    theirs.uid = None;
    let book = crate::agents::Book {
        socket_group: None,
        agents: vec![theirs],
    };
    let enc = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    state.files.insert(
        crate::agents::FILE.into(),
        enc(&serde_json::to_vec_pretty(&book).expect("book serializes")),
    );
    state.files.insert(
        FILE.into(),
        enc(&serde_json::to_vec_pretty(&Export {
            recipient: req.owner_recipient.clone(),
            places: req.places.clone(),
            next_seq: 1,
        })
        .expect("export serializes")),
    );
    state.add_file(home, &crate::policy::file_name(&agent.name));
    // Only this note's lease records travel.
    let records: String = crate::records::Records::new(home)
        .list()
        .into_iter()
        .filter(|r| Some(r.note_id) == entry.note_id)
        .filter_map(|r| serde_json::to_string(&r).ok())
        .map(|l| l + "\n")
        .collect();
    state
        .files
        .insert(crate::records::FILE.into(), enc(records.as_bytes()));
    state
        .to_bytes()
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// The agent's machine, after a lease settled: writes the note's new state, encrypted to the
/// owner and numbered, to every place. `Ok(None)` when this machine was not set up by a move.
pub fn export_state(
    home: &Path,
    vault: &WalletVault,
    commitment: &str,
    now_secs: u64,
) -> Result<Option<Vec<PathBuf>>, String> {
    let Some(mut export) = Export::load(home)? else {
        return Ok(None);
    };
    let files = vault
        .export_all()
        .map_err(|e| e.to_string())?
        .remove(commitment)
        .ok_or("the note has no wallet state")?;
    let state = decode_state(
        files
            .get(STATE_NAME)
            .ok_or("the wallet holds no note state")?,
    )?;
    let seq = export.next_seq;
    export.next_seq += 1;
    export.save(home)?;
    let payload = json!({
        "kind": KIND, "version": 1, "seq": seq, "taken_at": now_secs,
        "commitment": commitment, "state": state,
    });
    let recipient = backup::parse_recipient(&export.recipient).map_err(|e| e.to_string())?;
    let target = Target::new(recipient, export.places.clone());
    let name = format!(
        "moved-{}-{seq:010}.age",
        commitment
            .trim_start_matches("0x")
            .get(..8)
            .unwrap_or("note")
    );
    backup::write_backups(&target, payload.to_string().as_bytes(), &name)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn decode_state(b64: &str) -> Result<NoteState, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// What the import checks a state against.
pub struct Verify {
    pub operator_key: CurvePointWire,
    pub chain_id: u64,
    pub contract: Felt252,
}

/// Asks the operator whether it counts a nullifier as used (`0x` and 64 hex digits).
#[async_trait::async_trait]
pub trait SpentCheck: Send + Sync {
    async fn reported_spent(&self, nullifier: &str) -> Result<bool, String>;
}

pub struct OperatorSpent {
    pub base: String,
    pub http: reqwest::Client,
}

#[async_trait::async_trait]
impl SpentCheck for OperatorSpent {
    async fn reported_spent(&self, nullifier: &str) -> Result<bool, String> {
        let reply = self
            .http
            .get(format!(
                "{}/v2/nullifiers/{nullifier}",
                self.base.trim_end_matches('/')
            ))
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !reply.status().is_success() {
            return Err(format!("the operator answered {}", reply.status()));
        }
        let body: Value = reply
            .json()
            .await
            .map_err(|e| e.without_url().to_string())?;
        Ok(body["nullifier_status"]
            .as_str()
            .is_some_and(|s| s != "unknown"))
    }
}

#[derive(Debug)]
pub struct Picked {
    pub file: PathBuf,
    pub seq: u64,
    /// How many requests the state is past the deposit.
    pub position: u64,
}

#[derive(Debug, Default)]
pub struct Report {
    pub best: Option<Picked>,
    /// Files whose state is behind the best one.
    pub stale: Vec<Picked>,
    /// Files that were not used, with the reason.
    pub rejected: Vec<(PathBuf, String)>,
    pub warnings: Vec<String>,
    /// Whether the owner's wallet now holds the best state.
    pub installed: bool,
}

impl Report {
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.best {
            Some(b) => out.push(format!(
                "Best state: {} (sequence {}, {} request(s) after the deposit). {}",
                b.file.display(),
                b.seq,
                b.position,
                if self.installed {
                    "Your wallet now holds it."
                } else {
                    "Your wallet already held a state this recent, so nothing was changed."
                }
            )),
            None => out.push("No usable state found.".into()),
        }
        let best_pos = self.best.as_ref().map_or(0, |b| b.position);
        for s in &self.stale {
            out.push(format!(
                "Stale: {} (sequence {}) holds a state {} request(s) behind the best one; its sequence number does not change that.",
                s.file.display(),
                s.seq,
                best_pos - s.position
            ));
        }
        for (f, why) in &self.rejected {
            out.push(format!("Not used: {}: {why}", f.display()));
        }
        out.extend(self.warnings.iter().map(|w| format!("WARNING: {w}")));
        out
    }
}

struct Seen {
    file: PathBuf,
    seq: u64,
    state: NoteState,
}

/// Reads every file under `from`, keeps the states that are on the note's chain and signed by
/// the operator, and installs the one furthest along into the owner's wallet.
#[allow(clippy::too_many_arguments)]
pub async fn import_best(
    home: &Path,
    passphrase: &str,
    rounds: u32,
    agent: &str,
    from: &[PathBuf],
    key: &str,
    verify: &Verify,
    spent: &dyn SpentCheck,
) -> Result<Report, String> {
    let book = Registry::at(home).load()?;
    let agent = book
        .agents
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("no agent named {agent}"))?;
    let notes = NoteStore::open(&home.join("notes.sealed"), passphrase, rounds)
        .map_err(|e| e.to_string())?;
    let entry = notes
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|n| n.commitment == agent.note)
        .ok_or("the budget note is not in this state directory")?;
    let secret = Felt252::from_hex(&entry.secret)?;
    let note_id = entry.note_id.ok_or("the note has no id yet")?;
    let vault = WalletVault::new(home, passphrase, rounds);
    if vault.work_dir(&entry.commitment).is_ok_and(|d| d.exists()) {
        return Err("a lease is open on the note".into());
    }

    let mut report = Report::default();
    let mut seen: Vec<Seen> = Vec::new();
    for path in files_in(from) {
        match read_state(&path, key, &entry.commitment) {
            Ok((seq, state)) => seen.push(Seen {
                file: path,
                seq,
                state,
            }),
            Err(why) => report.rejected.push((path, why)),
        }
    }
    // The owner's own wallet competes too: it is never replaced by something older.
    let local = vault
        .export_all()
        .map_err(|e| e.to_string())?
        .remove(&entry.commitment)
        .unwrap_or_default();
    let local_state = local.get(STATE_NAME).and_then(|raw| decode_state(raw).ok());

    let wanted: Vec<Felt252> = seen
        .iter()
        .map(|s| s.state.current_anchor)
        .chain(local_state.iter().map(|s| s.current_anchor))
        .collect();
    let positions = positions_on_chain(&secret, note_id, &wanted);

    let mut valid: Vec<(Seen, u64)> = Vec::new();
    for s in seen {
        match check(
            &s.state,
            &secret,
            note_id,
            verify,
            &entry,
            positions.get(&s.state.current_anchor),
        ) {
            Ok(pos) => valid.push((s, pos)),
            Err(why) => report.rejected.push((s.file, why)),
        }
    }
    // The chain decides; the file's own sequence number only breaks a tie.
    valid.sort_by_key(|(s, pos)| (*pos, s.seq));
    let Some((best, best_pos)) = valid.pop() else {
        return Ok(report);
    };
    for (s, pos) in valid {
        if pos < best_pos {
            report.stale.push(Picked {
                file: s.file,
                seq: s.seq,
                position: pos,
            });
        }
    }
    let local_pos = local_state
        .as_ref()
        .and_then(|s| positions.get(&s.current_anchor).copied());
    if local_pos.is_none_or(|p| best_pos > p) {
        let mut files = local;
        files.insert(
            STATE_NAME.into(),
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec_pretty(&best.state).map_err(|e| e.to_string())?),
        );
        vault
            .import(&entry.commitment, &files)
            .map_err(|e| e.to_string())?;
        report.installed = true;
    }
    let nullifier = zkapi_core::v2::nullifier(&secret, note_id, &best.state.current_anchor);
    match spent
        .reported_spent(&format!("0x{}", hex::encode(nullifier.0)))
        .await
    {
        Ok(true) => report.warnings.push(
            "the operator already counts the nullifier of the best state as used, so a newer state exists that these files do not hold. Look for it in the places the agent writes to. Closing or exiting from this state can be defeated".into(),
        ),
        Ok(false) => {}
        Err(why) => report.warnings.push(format!(
            "could not ask the operator whether the best state is current ({why}); it may be behind"
        )),
    }
    report.best = Some(Picked {
        file: best.file,
        seq: best.seq,
        position: best_pos,
    });
    Ok(report)
}

fn files_in(from: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in from {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                let mut names: Vec<_> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|f| f.extension().is_some_and(|x| x == "age"))
                    .collect();
                names.sort();
                out.extend(names);
            }
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn read_state(path: &Path, key: &str, commitment: &str) -> Result<(u64, NoteState), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let plain = backup::decrypt(key, &bytes).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
    if v["kind"] != KIND || v["version"] != 1 {
        return Err("not a moved-note state".into());
    }
    if v["commitment"].as_str() != Some(commitment) {
        return Err("it belongs to another note".into());
    }
    let seq = v["seq"].as_u64().ok_or("no sequence number")?;
    let state = serde_json::from_value(v["state"].clone()).map_err(|e| e.to_string())?;
    Ok((seq, state))
}

/// The place of each wanted anchor on the note's chain: the anchor after `k` requests is
/// `next_anchor(nullifier(secret, note_id, anchor_{k-1}))`, from the genesis anchor 1.
fn positions_on_chain(secret: &Felt252, note_id: u32, wanted: &[Felt252]) -> HashMap<Felt252, u64> {
    let mut found = HashMap::new();
    let mut left: Vec<Felt252> = wanted.to_vec();
    let mut anchor = Felt252::ONE;
    for k in 0..=MAX_CHAIN {
        left.retain(|w| {
            let hit = *w == anchor;
            if hit {
                found.insert(*w, k);
            }
            !hit
        });
        if left.is_empty() {
            break;
        }
        anchor = zkapi_core::v2::next_anchor(&zkapi_core::v2::nullifier(secret, note_id, &anchor));
    }
    found
}

fn check(
    s: &NoteState,
    secret: &Felt252,
    note_id: u32,
    v: &Verify,
    entry: &NoteEntry,
    position: Option<&u64>,
) -> Result<u64, String> {
    if s.secret_s != *secret
        || s.note_id != note_id
        || s.chain_id != v.chain_id
        || s.contract_address != v.contract
    {
        return Err("it is not a state of this note on this vault".into());
    }
    if Some(s.deposit_amount.to_string()) != entry.deposit_amount {
        return Err("its deposit amount differs from the note's".into());
    }
    let pos = *position.ok_or("its anchor is not on this note's chain")?;
    if pos > 0 {
        let sig = s
            .state_signature
            .as_ref()
            .ok_or("it carries no operator signature")?;
        let message = zkapi_core::v2::state_message(
            s.protocol_version,
            s.chain_id,
            &s.contract_address,
            &s.current_commitment_x,
            &s.current_commitment_y,
            &s.current_anchor,
        );
        if !verify_signature(&v.operator_key, &message, sig).unwrap_or(false) {
            return Err("the operator's signature on it does not verify".into());
        }
    }
    Ok(pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentEntry, Book};
    use std::sync::atomic::{AtomicBool, Ordering};
    use zkapi_proof::compact::CompactSigner;

    const PASS: &str = "pass";
    const COMMITMENT: &str = "0xaabbccdd11223344";
    const CHAIN: u64 = 7;

    fn contract() -> Felt252 {
        Felt252::from_u64(99)
    }

    fn secret() -> Felt252 {
        Felt252::from_u64(1234)
    }

    fn signer() -> CompactSigner {
        CompactSigner::from_seed(&Felt252::from_u64(1))
    }

    fn verify() -> Verify {
        Verify {
            operator_key: signer().public_key(),
            chain_id: CHAIN,
            contract: contract(),
        }
    }

    /// The state after `k` requests, signed by `by`.
    fn state_at(k: u64, by: &CompactSigner) -> NoteState {
        let mut s = NoteState::new_from_deposit(
            2,
            CHAIN,
            contract(),
            3,
            secret(),
            1_000_000,
            1_900_000_000,
            "0x01".into(),
            Felt252::from_u64(5),
            Felt252::from_u64(6),
        );
        for _ in 0..k {
            s.current_anchor = zkapi_core::v2::next_anchor(&zkapi_core::v2::nullifier(
                &secret(),
                3,
                &s.current_anchor,
            ));
            s.is_genesis = false;
        }
        if k > 0 {
            let m = zkapi_core::v2::state_message(
                2,
                CHAIN,
                &contract(),
                &s.current_commitment_x,
                &s.current_commitment_y,
                &s.current_anchor,
            );
            s.state_signature = Some(by.sign(&m));
        }
        s.state_seq = k;
        s
    }

    struct Rig {
        home: tempfile::TempDir,
        places: tempfile::TempDir,
        owner_key: String,
        owner_pub: String,
    }

    impl Rig {
        /// The owner's state directory with the budget note `a` (at its deposit state).
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let notes = NoteStore::open(&home.path().join("notes.sealed"), PASS, 10).unwrap();
            notes
                .add(NoteEntry {
                    chain_label: "near-testnet".into(),
                    secret: hex::encode(secret().0),
                    commitment: COMMITMENT.into(),
                    denomination: "1000000".into(),
                    payout: "alice.test".into(),
                    status: NoteStatus::Accepted,
                    note_id: Some(3),
                    expiry_ns: Some(1_900_000_000),
                    deposit_amount: Some("1000000".into()),
                    tx_hash: None,
                    created_at: 1,
                })
                .unwrap();
            Registry::at(home.path())
                .update(|b: &mut Book| {
                    b.agents.push(AgentEntry {
                        name: "a".into(),
                        token_hash: "00".into(),
                        note: COMMITMENT.into(),
                        owner_account: "alice.test".into(),
                        policy_version: Some(1),
                        policy_seen: 1,
                        transport: Transport::Tcp,
                        uid: None,
                        disabled: false,
                        moved: true,
                        created_at: 1,
                    });
                    Ok(())
                })
                .unwrap();
            let (owner_key, owner_pub) = backup::generate_identity();
            Self {
                home,
                places: tempfile::tempdir().unwrap(),
                owner_key: owner_key.to_string(),
                owner_pub,
            }
        }

        /// What the agent's nymirad writes: a file holding `state`, numbered `seq`.
        fn write(&self, state: &NoteState, seq: u64, commitment: &str, to: &Path) -> PathBuf {
            let payload = json!({"kind": KIND, "version": 1, "seq": seq, "taken_at": 1,
                "commitment": commitment, "state": state});
            let target = Target::new(backup::parse_recipient(&self.owner_pub).unwrap(), vec![]);
            let file = to.join(format!("f-{seq:04}-{}.age", state.state_seq));
            std::fs::write(
                &file,
                backup::encrypt(&target, payload.to_string().as_bytes()).unwrap(),
            )
            .unwrap();
            file
        }

        async fn import(&self, spent: &dyn SpentCheck) -> Report {
            import_best(
                self.home.path(),
                PASS,
                10,
                "a",
                &[self.places.path().into()],
                &self.owner_key,
                &verify(),
                spent,
            )
            .await
            .unwrap()
        }

        fn wallet_state(&self) -> Option<NoteState> {
            let vault = WalletVault::new(self.home.path(), PASS, 10);
            vault
                .export_all()
                .unwrap()
                .remove(COMMITMENT)
                .and_then(|f| f.get(STATE_NAME).and_then(|r| decode_state(r).ok()))
        }
    }

    struct Spent(bool);

    #[async_trait::async_trait]
    impl SpentCheck for Spent {
        async fn reported_spent(&self, _: &str) -> Result<bool, String> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn a_file_with_a_high_sequence_number_and_an_older_state_loses_to_the_longer_chain() {
        let rig = Rig::new();
        let s = signer();
        for k in 1..=3 {
            rig.write(&state_at(k, &s), k, COMMITMENT, rig.places.path());
        }
        // Validly sealed, the highest sequence number, but the state after one request.
        let old = rig.write(&state_at(1, &s), 999, COMMITMENT, rig.places.path());
        let report = rig.import(&Spent(false)).await;
        let best = report.best.as_ref().unwrap();
        assert_eq!((best.position, best.seq), (3, 3), "{:?}", report.lines());
        assert!(report.stale.iter().any(|p| p.file == old && p.seq == 999));
        assert_eq!(
            rig.wallet_state().unwrap().current_anchor,
            state_at(3, &s).current_anchor
        );
        assert!(report
            .lines()
            .iter()
            .any(|l| l.starts_with("Stale:") && l.contains("-0999-")));
    }

    #[tokio::test]
    async fn the_sequence_number_only_breaks_a_tie_between_equal_chain_positions() {
        let rig = Rig::new();
        let s = signer();
        rig.write(&state_at(2, &s), 5, COMMITMENT, rig.places.path());
        let high = rig.write(&state_at(2, &s), 6, COMMITMENT, rig.places.path());
        let report = rig.import(&Spent(false)).await;
        assert_eq!(report.best.unwrap().file, high);
        assert!(report.stale.is_empty());
    }

    #[tokio::test]
    async fn a_state_the_operator_reports_spent_triggers_the_warning() {
        let rig = Rig::new();
        rig.write(&state_at(2, &signer()), 1, COMMITMENT, rig.places.path());
        let quiet = rig.import(&Spent(false)).await;
        assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
        let loud = rig.import(&Spent(true)).await;
        assert!(
            loud.warnings
                .iter()
                .any(|w| w.contains("already counts the nullifier")),
            "{:?}",
            loud.warnings
        );
        assert!(loud.lines().iter().any(|l| l.starts_with("WARNING:")));
    }

    #[tokio::test]
    async fn the_operators_signature_decides_and_forgeries_are_named() {
        let rig = Rig::new();
        let fake = CompactSigner::from_seed(&Felt252::from_u64(2));
        let forged = rig.write(&state_at(4, &fake), 9, COMMITMENT, rig.places.path());
        let other = rig.write(&state_at(2, &signer()), 8, "0xdeadbeef", rig.places.path());
        let mut off = state_at(2, &signer());
        off.current_anchor = Felt252::from_u64(31337);
        let off = rig.write(&off, 7, COMMITMENT, rig.places.path());
        let good = rig.write(&state_at(1, &signer()), 1, COMMITMENT, rig.places.path());
        let report = rig.import(&Spent(false)).await;
        assert_eq!(report.best.as_ref().unwrap().file, good);
        let why = |f: &PathBuf| {
            report
                .rejected
                .iter()
                .find(|(p, _)| p == f)
                .unwrap()
                .1
                .clone()
        };
        assert!(why(&forged).contains("signature"), "{}", why(&forged));
        assert!(why(&other).contains("another note"));
        assert!(why(&off).contains("not on this note's chain"));
    }

    #[tokio::test]
    async fn a_wallet_that_is_further_along_is_not_replaced() {
        let rig = Rig::new();
        let s = signer();
        let vault = WalletVault::new(rig.home.path(), PASS, 10);
        let ahead = serde_json::to_vec(&state_at(5, &s)).unwrap();
        vault
            .import(
                COMMITMENT,
                &Files::from([(
                    STATE_NAME.to_string(),
                    base64::engine::general_purpose::STANDARD.encode(ahead),
                )]),
            )
            .unwrap();
        rig.write(&state_at(2, &s), 1, COMMITMENT, rig.places.path());
        let report = rig.import(&Spent(false)).await;
        assert!(!report.installed);
        assert_eq!(rig.wallet_state().unwrap().state_seq, 5);
    }

    #[tokio::test]
    async fn another_key_opens_nothing() {
        let rig = Rig::new();
        let f = rig.write(&state_at(1, &signer()), 1, COMMITMENT, rig.places.path());
        let (stranger, _) = backup::generate_identity();
        let report = import_best(
            rig.home.path(),
            PASS,
            10,
            "a",
            &[rig.places.path().into()],
            &stranger,
            &verify(),
            &Spent(false),
        )
        .await
        .unwrap();
        assert!(report.best.is_none());
        assert_eq!(report.rejected[0].0, f);
    }

    #[test]
    fn the_warning_and_the_race_say_what_they_must() {
        assert!(WARNING.contains("revocation is not guaranteed, the ceiling is"));
        for part in [
            "policy",
            "counters",
            "approvals",
            "agent's machine",
            "advisory",
        ] {
            assert!(WARNING.contains(part), "{part}");
        }
        for part in [
            "race",
            "Only what is unspent when your final close or exit lands goes to your payout account",
            "A hostile agent can spend the rest on inference first",
            "An exit from a stale state can be defeated by a newer one",
        ] {
            assert!(RACE.contains(part), "{part}");
        }
    }

    #[test]
    fn a_declined_confirmation_moves_nothing() {
        let rig = Rig::new();
        Registry::at(rig.home.path())
            .update(|b| {
                b.agents[0].moved = false;
                Ok(())
            })
            .unwrap();
        let asked = AtomicBool::new(false);
        let (_, agent_pub) = backup::generate_identity();
        let out = rig.places.path().join("move.age");
        let req = MoveRequest {
            agent: "a".into(),
            agent_recipient: agent_pub,
            owner_recipient: rig.owner_pub.clone(),
            places: vec![rig.places.path().into()],
            out: out.clone(),
        };
        let err = move_note(rig.home.path(), PASS, 10, &req, 1, &mut |q| {
            asked.store(true, Ordering::SeqCst);
            assert!(q.contains(WARNING) && q.contains(RACE));
            false
        })
        .unwrap_err();
        assert_eq!(err, "not moved");
        assert!(asked.load(Ordering::SeqCst));
        assert!(!out.exists());
        assert!(!Registry::at(rig.home.path()).load().unwrap().agents[0].moved);
    }
}
