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
use zkapi_core::v2::{next_anchor, state_message};
use zkapi_proof::compact::{
    random_scalar, server_update, validate_operator_signing_seeds, CompactSigner,
};
use zkapi_types::wire::{ApiRequestV2, RequestResponseV2};
use zkapi_types::{canonical_response_hash, Felt252, PROTOCOL_VERSION};

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
        })
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
        if due && self.heartbeat.send().await.is_ok() {
            *self
                .last_heartbeat
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(now);
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
        let json =
            serde_json::to_string(&response).map_err(|e| SignerError::Internal(e.to_string()))?;
        let (id, n, cri, now) = (
            lease_id.clone(),
            nullifier,
            original.client_request_id,
            self.clock.now_secs(),
        );
        let stored = json.clone();
        // On disk (synchronous=FULL) before the response leaves.
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
}

/// Binds the signer's socket, readable and writable by its owner only.
pub fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Ok(Box<RequestResponseV2>),
    Refused(String),
    Error(String),
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
    BufReader::new(read.take(MAX_REQUEST_BYTES))
        .read_line(&mut line)
        .await?;
    let reply = match serde_json::from_str::<SignRequest>(&line) {
        Err(e) => Reply::Refused(format!("not a typed request: {e}")),
        Ok(request) => match signer.sign_next(request).await {
            Ok(response) => Reply::Ok(Box::new(response)),
            Err(SignerError::Refused(why)) => Reply::Refused(why),
            Err(SignerError::Internal(why)) => Reply::Error(why),
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
        tokio::time::timeout(std::time::Duration::from_secs(10), self.call(request))
            .await
            .map_err(|_| SignerError::Internal("the signer did not answer in time".into()))?
    }
}

impl SocketSigner {
    async fn call(&self, request: SignRequest) -> Result<RequestResponseV2, SignerError> {
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
            Ok(Reply::Ok(response)) => Ok(*response),
            Ok(Reply::Refused(why)) => Err(SignerError::Refused(why)),
            Ok(Reply::Error(why)) => Err(SignerError::Internal(why)),
            Err(_) => Err(SignerError::Internal(
                "the signer closed the connection".into(),
            )),
        }
    }
}
