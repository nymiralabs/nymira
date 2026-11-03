//! The signer: holds the state key and the clearance key and signs the next state of a note.
//!
//! It trusts nothing it is handed. A typed request names a lease and a charge; the signer reads
//! the reservation from its own handle on the store, and signs only when the lease is retired,
//! the usage record is signed by the minting process's key, the charge is the one the vault's
//! parameters give for that record, and the nullifier is in no exit. The signed state is on disk
//! before the response leaves.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use zkapi_core::v2::{clearance_message, next_anchor, state_message};
use zkapi_proof::compact::{
    random_scalar, server_update, validate_operator_signing_seeds, CompactSigner,
};
use zkapi_types::wire::{ApiRequestV2, RequestResponseV2};
use zkapi_types::{canonical_response_hash, Felt252, SchnorrSignature, PROTOCOL_VERSION};

use crate::chain::VaultView;
use crate::charge::{charge, ChargeParams};
use crate::clock::Clock;
use crate::lifecycle::USAGE_WAIT_SECS;
use crate::store::{self, Store};
use crate::usage::{verify, SignedUsage};

/// The heartbeat is sent this often.
pub const HEARTBEAT_EVERY_SECS: u64 = 30 * 60;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    #[error("refused: {0}")]
    Refused(String),
    #[error("signer failed: {0}")]
    Internal(String),
    /// The state could not be put on the second machine, so it was not released.
    #[error("the replica did not acknowledge")]
    ReplicaUnreachable,
}

fn refused(reason: &str) -> SignerError {
    SignerError::Refused(reason.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignRequest {
    pub lease_id: String,
    /// Decimal micro-USD.
    pub charge: String,
    pub usage: SignedUsage,
}

#[async_trait]
pub trait SignerApi: Send + Sync {
    async fn sign_next(&self, request: SignRequest) -> Result<RequestResponseV2, SignerError>;
    /// Signs a clearance for a nullifier the operator has reserved for one: protocol version,
    /// chain, vault and nullifier, under the clearance key.
    async fn sign_clearance(&self, nullifier: Felt252) -> Result<SchnorrSignature, SignerError>;
}

#[async_trait]
pub trait HeartbeatSender: Send + Sync {
    /// Sends `heartbeat` from the operator account.
    async fn send(&self) -> Result<(), String>;
}

/// Counts heartbeats; can be switched to fail.
#[derive(Default)]
pub struct FakeHeartbeat {
    sent: Mutex<u32>,
    fail: Mutex<bool>,
}

impl FakeHeartbeat {
    pub fn sent(&self) -> u32 {
        *self.sent.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_failing(&self, fail: bool) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = fail;
    }
}

#[async_trait]
impl HeartbeatSender for FakeHeartbeat {
    async fn send(&self) -> Result<(), String> {
        if *self.fail.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err("rpc down".into());
        }
        *self.sent.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        Ok(())
    }
}

pub struct Signer {
    state: CompactSigner,
    clearance: CompactSigner,
    store: Store,
    vault: Arc<dyn VaultView>,
    minter: VerifyingKey,
    chain_id: u64,
    contract: Felt252,
    clock: Arc<dyn Clock>,
    heartbeat: Arc<dyn HeartbeatSender>,
    last_heartbeat: Mutex<Option<u64>>,
    /// Failed heartbeat sends in a row and the earliest time of the next try.
    heartbeat_retry: Mutex<(u32, u64)>,
    status_host: Option<String>,
    /// Every signed state is here before it is stored where a client can read it.
    replica: Arc<dyn crate::replica::Replica>,
}

/// A 31-byte seed from a passphrase (PBKDF2-HMAC-SHA256, salted per role), so two roles with
/// different passphrases get unrelated keys. The top byte is zero: always a canonical felt.
pub fn seed_from_passphrase(passphrase: &str, role: &str, rounds: u32) -> Felt252 {
    let mut out = [0u8; 32];
    let salt = format!("nymira/signer/v1/{role}");
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
        passphrase.as_bytes(),
        salt.as_bytes(),
        rounds,
        &mut out[1..],
    );
    Felt252(out)
}

/// Both seeds from two passphrases. Equal passphrases are refused: separate secrets are the
/// point of two keys.
pub fn seeds_from_passphrases(
    state_passphrase: &str,
    clearance_passphrase: &str,
    rounds: u32,
) -> Result<(Felt252, Felt252), SignerError> {
    if state_passphrase == clearance_passphrase {
        return Err(SignerError::Internal(
            "the two passphrases must differ".into(),
        ));
    }
    Ok((
        seed_from_passphrase(state_passphrase, "state", rounds),
        seed_from_passphrase(clearance_passphrase, "clearance", rounds),
    ))
}

/// Fewest characters of a signing passphrase outside the sandbox. The public keys sit in the
/// vault's `config()`, so anyone can test passphrases against them offline; the KDF slows each
/// guess but only length stops the search.
pub const MIN_PASSPHRASE_CHARS: usize = 20;

/// Refuses a passphrase that is too short to be a signing secret.
pub fn check_passphrase_strength(passphrase: &str) -> Result<(), SignerError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(SignerError::Internal(format!(
            "a signing passphrase needs at least {MIN_PASSPHRASE_CHARS} characters"
        )));
    }
    Ok(())
}

/// The state and clearance public keys two passphrases give: what `new()` of the vault takes as
/// `operator_pk` and `clearance_pk`.
pub fn public_keys_from_passphrases(
    state_passphrase: &str,
    clearance_passphrase: &str,
    rounds: u32,
    allow_development: bool,
) -> Result<
    (
        zkapi_types::wire::CurvePointWire,
        zkapi_types::wire::CurvePointWire,
    ),
    SignerError,
> {
    if !allow_development {
        check_passphrase_strength(state_passphrase)?;
        check_passphrase_strength(clearance_passphrase)?;
    }
    let (state_seed, clearance_seed) =
        seeds_from_passphrases(state_passphrase, clearance_passphrase, rounds)?;
    let (state, clearance) =
        Signer::signers_from_seeds(&state_seed, &clearance_seed, allow_development)?;
    Ok((state.public_key(), clearance.public_key()))
}

/// Rounds of the passphrase KDF in production.
pub const KDF_ROUNDS: u32 = 600_000;

impl Signer {
    /// Refuses to build when the two public keys are equal: a clearance would then be a state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: CompactSigner,
        clearance: CompactSigner,
        store: Store,
        vault: Arc<dyn VaultView>,
        minter: VerifyingKey,
        chain_id: u64,
        contract: Felt252,
        clock: Arc<dyn Clock>,
        heartbeat: Arc<dyn HeartbeatSender>,
    ) -> Result<Self, SignerError> {
        if state.public_key() == clearance.public_key() {
            return Err(SignerError::Internal(
                "state and clearance keys are equal; use independent seeds".into(),
            ));
        }
        Ok(Self {
            state,
            clearance,
            store,
            vault,
            minter,
            chain_id,
            contract,
            clock,
            heartbeat,
            last_heartbeat: Mutex::new(None),
            heartbeat_retry: Mutex::new((0, 0)),
            status_host: None,
            replica: Arc::new(crate::replica::NoReplica),
        })
    }

    /// The second machine every signed state goes to first.
    pub fn with_replica(mut self, replica: Arc<dyn crate::replica::Replica>) -> Self {
        self.replica = replica;
        self
    }

    /// Two seeds, checked by zkapi's operator policy (no development keys, no weak secrets)
    /// unless `allow_development` (sandbox and rehearsal builds).
    pub fn signers_from_seeds(
        state_seed: &Felt252,
        clearance_seed: &Felt252,
        allow_development: bool,
    ) -> Result<(CompactSigner, CompactSigner), SignerError> {
        if allow_development {
            let state = CompactSigner::try_from_seed(state_seed)
                .map_err(|e| SignerError::Internal(e.to_string()))?;
            let clearance = CompactSigner::try_from_seed(clearance_seed)
                .map_err(|e| SignerError::Internal(e.to_string()))?;
            return Ok((state, clearance));
        }
        validate_operator_signing_seeds(state_seed, clearance_seed)
            .map_err(|e| SignerError::Internal(e.to_string()))
    }

    /// Signs a status nonce for the host this signer was told to serve, for the current slot or
    /// the one before it. Nothing else is signed with this domain.
    pub fn sign_status(
        &self,
        nonce: &str,
        host: &str,
        ts: u64,
    ) -> Result<SchnorrSignature, String> {
        let slot = crate::status::SLOT_SECS;
        let now = self.clock.now_secs();
        if self.status_host.as_deref() != Some(host) {
            return Err("not the host this signer serves".into());
        }
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err("a status nonce is 32 lowercase hex digits".into());
        }
        if !ts.is_multiple_of(slot) || ts > now + slot || ts + 3 * slot < now {
            return Err("the slot is not a recent one".into());
        }
        Ok(self
            .state
            .sign(&crate::status::status_message(nonce, host, ts)))
    }

    /// The host name status nonces are signed for; without it no status is signed.
    pub fn with_status_host(mut self, host: &str) -> Self {
        self.status_host = Some(host.into());
        self
    }

    pub fn state_public_key(&self) -> zkapi_types::wire::CurvePointWire {
        self.state.public_key()
    }

    /// The clearance key signs clearances only, never states.
    pub fn clearance_public_key(&self) -> zkapi_types::wire::CurvePointWire {
        self.clearance.public_key()
    }

    /// Sends the heartbeat when 30 minutes have passed since the last one that went out.
    pub async fn tick(&self) {
        let now = self.clock.now_secs();
        let due = self
            .last_heartbeat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none_or(|at| now.saturating_sub(at) >= HEARTBEAT_EVERY_SECS);
        let wait_until = self
            .heartbeat_retry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1;
        if !due || now < wait_until {
            return;
        }
        let sent = self.heartbeat.send().await.is_ok();
        let mut retry = self
            .heartbeat_retry
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if sent {
            *retry = (0, 0);
            *self
                .last_heartbeat
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(now);
        } else {
            // 60 s, doubling to 10 minutes: a node that is down is not asked every ten seconds.
            let failures = retry.0.saturating_add(1);
            let wait = (60u64 << (failures - 1).min(4)).min(600);
            *retry = (failures, now + wait);
        }
    }

    /// Runs [`Signer::tick`] every ten seconds for as long as the process lives.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let signer = self.clone();
        tokio::spawn(async move {
            loop {
                signer.tick().await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        })
    }

    async fn params(&self) -> Result<ChargeParams, SignerError> {
        let config = self
            .vault
            .config()
            .await
            .map_err(|e| SignerError::Internal(e.to_string()))?;
        Ok(ChargeParams::from(&config))
    }
}

fn internal(e: rusqlite::Error) -> SignerError {
    SignerError::Internal(e.to_string())
}

#[async_trait]
impl SignerApi for Signer {
    async fn sign_clearance(&self, nullifier: Felt252) -> Result<SchnorrSignature, SignerError> {
        let n = crate::operator::felt_hex(&nullifier);
        let (row, exit, lease) = self
            .store
            .run(move |c| {
                Ok((
                    store::nullifier_row(c, &n)?,
                    store::is_exit_seen(c, &n)?,
                    store::lease_by_nullifier(c, &n)?,
                ))
            })
            .await
            .map_err(internal)?;
        if exit || lease.is_some() {
            return Err(refused("the nullifier is in an exit or holds a lease"));
        }
        if row.is_none_or(|r| r.state != store::state::CLEARANCE_RESERVED) {
            return Err(refused("no clearance is reserved for this nullifier"));
        }
        let message =
            clearance_message(PROTOCOL_VERSION, self.chain_id, &self.contract, &nullifier);
        Ok(self.clearance.sign(&message))
    }

    async fn sign_next(&self, request: SignRequest) -> Result<RequestResponseV2, SignerError> {
        let lease_id = request.lease_id.clone();
        let id = lease_id.clone();
        let (lease, signed, request_json) = self
            .store
            .run(move |c| {
                Ok((
                    store::lease(c, &id)?,
                    store::signed_state(c, &id)?,
                    store::request_json(c, &id)?,
                ))
            })
            .await
            .map_err(internal)?;
        let lease = lease.ok_or_else(|| refused("no reservation for this lease"))?;
        let claimed: u128 = request
            .charge
            .parse()
            .map_err(|_| refused("charge is not a number"))?;
        if let Some((_, signed_charge, json)) = signed {
            // Asked again for a state already signed: the same state, only for the same charge.
            if signed_charge != claimed {
                return Err(refused("a state with another charge is already signed"));
            }
            return serde_json::from_str(&json).map_err(|e| SignerError::Internal(e.to_string()));
        }
        if lease.status != "retired" {
            return Err(refused("the lease is not retired"));
        }
        let stored: Option<SignedUsage> = lease
            .usage_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok());
        if stored.as_ref().map(|u| &u.record) != Some(&request.usage.record) {
            return Err(refused(
                "the usage record is not the one stored for this lease",
            ));
        }
        if request.usage.record.read_at
            < lease
                .revoked_at
                .unwrap_or(u64::MAX)
                .saturating_add(USAGE_WAIT_SECS)
        {
            return Err(refused("the usage was read before the key had settled"));
        }
        if !verify(&self.minter, &request.usage) || request.usage.record.lease_id != lease_id {
            return Err(refused(
                "the usage record is not signed by the minting process for this lease",
            ));
        }
        let nullifier = lease.nullifier.clone();
        let n = nullifier.clone();
        let (row, exit) = self
            .store
            .run(move |c| Ok((store::nullifier_row(c, &n)?, store::is_exit_seen(c, &n)?)))
            .await
            .map_err(internal)?;
        if exit
            || row
                .as_ref()
                .is_none_or(|r| r.state != store::state::RESERVED)
        {
            return Err(refused(
                "the nullifier is in an exit or is not reserved for this lease",
            ));
        }
        let params = self.params().await?;
        let held = lease.held_secs();
        let expected = match &request.usage.record.cost_nano_usd {
            Some(cost) => charge(
                lease.bound,
                cost.parse().map_err(|_| refused("usage is not a number"))?,
                held,
                params,
            ),
            None => lease.bound,
        };
        if claimed != expected {
            return Err(refused("the charge does not match the usage record"));
        }
        let original: ApiRequestV2 =
            serde_json::from_str(&request_json.ok_or_else(|| refused("no request proof"))?)
                .map_err(|e| SignerError::Internal(e.to_string()))?;
        let inputs = &original.public_inputs;
        // A retry after a crash or a timeout signs the state the replica already holds, not
        // another one with a fresh blind.
        let held = self
            .replica
            .recorded(crate::replog::kinds::SIGNED, &lease_id)
            .await;
        let (response, json) = match held {
            Some(json) => {
                let response: RequestResponseV2 = serde_json::from_str(&json)
                    .map_err(|e| SignerError::Internal(e.to_string()))?;
                if response.charge_applied != expected {
                    return Err(refused("a state with another charge is already signed"));
                }
                (response, json)
            }
            None => {
                let anonymous = zkapi_types::wire::CurvePointWire {
                    x: inputs.anonymous_commitment_x,
                    y: inputs.anonymous_commitment_y,
                };
                let blind_delta = random_scalar();
                let next_commitment = server_update(&anonymous, expected, &blind_delta)
                    .map_err(|e| SignerError::Internal(e.to_string()))?;
                let anchor = next_anchor(&inputs.request_nullifier);
                let message = state_message(
                    PROTOCOL_VERSION,
                    self.chain_id,
                    &self.contract,
                    &next_commitment.x,
                    &next_commitment.y,
                    &anchor,
                );
                let payload = "{}".to_string();
                let response = RequestResponseV2 {
                    status: "finalized".into(),
                    client_request_id: original.client_request_id.clone(),
                    request_nullifier: inputs.request_nullifier,
                    response_code: 200,
                    response_hash: canonical_response_hash(payload.as_bytes()),
                    response_payload: payload,
                    charge_applied: expected,
                    next_commitment,
                    next_anchor: anchor,
                    blind_delta_srv: blind_delta,
                    next_state_signature: self.state.sign(&message),
                    policy_reason_code: None,
                    policy_evidence_hash: None,
                };
                let json = serde_json::to_string(&response)
                    .map_err(|e| SignerError::Internal(e.to_string()))?;
                (response, json)
            }
        };
        let (id, n, cri, now) = (
            lease_id.clone(),
            nullifier,
            original.client_request_id,
            self.clock.now_secs(),
        );
        let stored = json.clone();
        // On the second machine first, then on disk (synchronous=FULL), then out.
        self.replica
            .record(crate::replog::kinds::SIGNED, &lease_id, Some(&json))
            .await
            .map_err(|_| SignerError::ReplicaUnreachable)?;
        let written = self
            .store
            .run(move |c| {
                if store::insert_signed_state(c, &id, &n, &cri, expected, &stored, now)? {
                    Ok(None)
                } else {
                    store::signed_state(c, &id).map(|s| s.map(|(_, _, j)| j))
                }
            })
            .await
            .map_err(internal)?;
        match written {
            None => Ok(response),
            Some(existing) => {
                serde_json::from_str(&existing).map_err(|e| SignerError::Internal(e.to_string()))
            }
        }
    }
}

/// Calls a signer in the same process.
pub struct LocalSigner(pub Arc<Signer>);

#[async_trait]
impl SignerApi for LocalSigner {
    async fn sign_next(&self, request: SignRequest) -> Result<RequestResponseV2, SignerError> {
        self.0.sign_next(request).await
    }

    async fn sign_clearance(&self, nullifier: Felt252) -> Result<SchnorrSignature, SignerError> {
        self.0.sign_clearance(nullifier).await
    }
}

/// Binds the signer's socket, readable and writable by its owner only.
pub fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// What a caller may ask: the next state of a retired lease, or a clearance.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Call {
    Next(SignRequest),
    Clearance {
        nullifier: Felt252,
    },
    /// A status nonce to sign under the state key: the signer builds the message itself.
    Status {
        nonce: String,
        host: String,
        ts: u64,
    },
    /// The state key's public key.
    StateKey,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Ok(Box<RequestResponseV2>),
    Clearance(SchnorrSignature),
    Status(SchnorrSignature),
    StateKey(zkapi_types::wire::CurvePointWire),
    Refused(String),
    Error(String),
    ReplicaUnreachable,
}

/// Whether `line` is a request the signer would act on: the wire check, with no signing.
pub fn is_typed_request(line: &str) -> bool {
    serde_json::from_str::<Call>(line).is_ok()
}

/// Serves typed requests, one JSON line each. A peer whose user id is not `allowed_uid` is
/// dropped before a byte is read. The listener is a Unix socket: there is no TCP entry.
pub async fn serve(listener: UnixListener, signer: Arc<Signer>, allowed_uid: u32) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(allowed_uid) {
            continue;
        }
        let signer = signer.clone();
        tokio::spawn(async move {
            let _ = handle(stream, signer).await;
        });
    }
}

async fn handle(stream: UnixStream, signer: Arc<Signer>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    // A peer that connects and says nothing must not hold the task.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        BufReader::new(read.take(MAX_REQUEST_BYTES)).read_line(&mut line),
    )
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let reply = match serde_json::from_str::<Call>(&line) {
        Err(e) => Reply::Refused(format!("not a typed request: {e}")),
        Ok(Call::Next(request)) => match signer.sign_next(request).await {
            Ok(response) => Reply::Ok(Box::new(response)),
            Err(SignerError::Refused(why)) => Reply::Refused(why),
            Err(SignerError::Internal(why)) => Reply::Error(why),
            Err(SignerError::ReplicaUnreachable) => Reply::ReplicaUnreachable,
        },
        Ok(Call::Status { nonce, host, ts }) => match signer.sign_status(&nonce, &host, ts) {
            Ok(signature) => Reply::Status(signature),
            Err(why) => Reply::Refused(why),
        },
        Ok(Call::StateKey) => Reply::StateKey(signer.state_public_key()),
        Ok(Call::Clearance { nullifier }) => match signer.sign_clearance(nullifier).await {
            Ok(signature) => Reply::Clearance(signature),
            Err(SignerError::Refused(why)) => Reply::Refused(why),
            Err(SignerError::Internal(why)) => Reply::Error(why),
            Err(SignerError::ReplicaUnreachable) => Reply::ReplicaUnreachable,
        },
    };
    let mut bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await
}

/// Calls a signer process over its Unix socket.
pub struct SocketSigner(pub PathBuf);

#[async_trait]
impl SignerApi for SocketSigner {
    async fn sign_next(&self, request: SignRequest) -> Result<RequestResponseV2, SignerError> {
        match self.ask(Call::Next(request)).await? {
            Reply::Ok(response) => Ok(*response),
            _ => Err(SignerError::Internal(
                "the signer sent another answer".into(),
            )),
        }
    }

    async fn sign_clearance(&self, nullifier: Felt252) -> Result<SchnorrSignature, SignerError> {
        match self.ask(Call::Clearance { nullifier }).await? {
            Reply::Clearance(signature) => Ok(signature),
            _ => Err(SignerError::Internal(
                "the signer sent another answer".into(),
            )),
        }
    }
}

impl SocketSigner {
    async fn ask(&self, call: Call) -> Result<Reply, SignerError> {
        tokio::time::timeout(std::time::Duration::from_secs(10), self.call(call))
            .await
            .map_err(|_| SignerError::Internal("the signer did not answer in time".into()))?
    }

    async fn call(&self, request: Call) -> Result<Reply, SignerError> {
        let io = |e: std::io::Error| SignerError::Internal(e.to_string());
        let stream = UnixStream::connect(&self.0).await.map_err(io)?;
        let (read, mut write) = stream.into_split();
        let mut bytes =
            serde_json::to_vec(&request).map_err(|e| SignerError::Internal(e.to_string()))?;
        bytes.push(b'\n');
        write.write_all(&bytes).await.map_err(io)?;
        let mut line = String::new();
        BufReader::new(read.take(MAX_REQUEST_BYTES * 4))
            .read_line(&mut line)
            .await
            .map_err(io)?;
        match serde_json::from_str::<Reply>(&line) {
            Ok(Reply::Refused(why)) => Err(SignerError::Refused(why)),
            Ok(Reply::Error(why)) => Err(SignerError::Internal(why)),
            Ok(Reply::ReplicaUnreachable) => Err(SignerError::ReplicaUnreachable),
            Ok(reply) => Ok(reply),
            Err(_) => Err(SignerError::Internal(
                "the signer closed the connection".into(),
            )),
        }
    }
}

/// The state key as the public API process sees it: the public key fetched once, signatures
/// asked of the signer per status slot.
pub struct SocketStateSigner {
    socket: SocketSigner,
    pk: zkapi_types::wire::CurvePointWire,
}

impl SocketStateSigner {
    pub async fn connect(path: &Path) -> Result<Self, SignerError> {
        let socket = SocketSigner(path.into());
        match socket.ask(Call::StateKey).await? {
            Reply::StateKey(pk) => Ok(Self { socket, pk }),
            _ => Err(SignerError::Internal(
                "the signer sent another answer".into(),
            )),
        }
    }
}

#[async_trait]
impl crate::status::StateSigning for SocketStateSigner {
    fn public_key(&self) -> zkapi_types::wire::CurvePointWire {
        self.pk.clone()
    }

    async fn sign_status(&self, nonce: &str, host: &str, ts: u64) -> Option<SchnorrSignature> {
        match self
            .socket
            .ask(Call::Status {
                nonce: nonce.into(),
                host: host.into(),
                ts,
            })
            .await
        {
            Ok(Reply::Status(signature)) => Some(signature),
            _ => None,
        }
    }
}
