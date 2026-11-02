//! The closer: takes a kind-1 close proof from the public API over a Unix socket, repeats the
//! contract's checks and sends `mutual_close` from the operator account.
//!
//! The checks run in a fixed order and before any gas is spent: field lengths,
//! the public inputs, `valid_until`, the note and its nullifier read from the vault at a final
//! block, any pending exit, then the Groth16 proof. The clearance signature is checked by the
//! proof itself, so the closer looks up no clearance table. The public API holds no NEAR key.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use zkapi_types::wire::{Groth16ProofWire, ProofBackendWire};
use zkapi_types::{Felt252, WithdrawalPublicInputsV2C, PROTOCOL_VERSION};

use crate::alarm::Alarms;
use crate::budget::SECS_PER_DAY;
use crate::chain::VaultView;
use crate::clock::Clock;
use crate::error::Code;
use crate::json::{only_keys, parse_strict};
use crate::operator::felt_hex;
use crate::store::{self, Store};
use crate::tx::{NearTx, TxRequest};
use crate::verify::WithdrawalProofVerifier;

pub const CLOSE_BODY_LIMIT: usize = 64 * 1024;
/// A close whose proof ends sooner than this is refused: it could lapse before it lands.
pub const MIN_VALID_SECS: u64 = 600;
/// Verifications at once.
pub const VERIFY_POOL: usize = 2;
pub const VERIFIES_PER_MINUTE: usize = 20;
pub const SENDS_PER_DAY: u64 = 50;
/// Operator-sent closes in a day that raise an alarm.
pub const SENDS_ALARM: u64 = 30;
/// Gas attached to `mutual_close`: the vault wants 80 Tgas, the close and its payout burn about 60.
pub const CLOSE_GAS: u64 = 100_000_000_000_000;
/// Longest the HTTP answer waits for the chain; the transaction goes on without it.
pub const ANSWER_WAIT_SECS: u64 = 20;
/// Public inputs of a withdrawal proof.
const INPUTS: usize = 16;
const PROOF_BYTES: usize = 256;

/// A refusal: the code, the text and, on a 429 or 503, when to try again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseError {
    pub code: String,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl CloseError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str().into(),
            message: message.into(),
            retry_after_secs: None,
        }
    }

    fn retry(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    pub fn code(&self) -> Code {
        Code::parse(&self.code).unwrap_or(Code::ProviderError)
    }
}

/// What the public API calls: the raw body goes in, the transaction hash comes out.
#[async_trait]
pub trait CloseApi: Send + Sync {
    async fn close(&self, body: &[u8]) -> Result<String, CloseError>;
}

/// The part of the closer that touches the chain account: it knows what was sent, and sends.
#[async_trait]
pub trait CloseSender: Send + Sync {
    /// The hash of a close already sent for this nullifier inside the repeat window.
    async fn existing(&self, nullifier: &str) -> Result<Option<String>, String>;
    /// Closes sent at or after `since`.
    async fn sent_since(&self, since: u64) -> Result<u64, String>;
    /// Sends `mutual_close(proof, inputs)`; returns the hash once the signed bytes are on disk
    /// and the broadcast has begun.
    async fn send(
        &self,
        nullifier: &str,
        proof: Vec<u8>,
        inputs: Vec<Vec<u8>>,
    ) -> Result<String, String>;
}

/// Sends through [`NearTx`] with the nullifier as the repeat key.
pub struct NearCloseSender {
    pub tx: Arc<NearTx>,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    /// Told when a close fails after the client already has its answer.
    pub alarms: Arc<Alarms>,
}

#[async_trait]
impl CloseSender for NearCloseSender {
    async fn existing(&self, nullifier: &str) -> Result<Option<String>, String> {
        let (key, since) = (
            nullifier.to_string(),
            self.clock
                .now_secs()
                .saturating_sub(crate::tx::IDEM_WINDOW_SECS),
        );
        let row = self
            .store
            .run(move |c| store::tx_by_idem_key(c, "close", &key, since))
            .await
            .map_err(|e| e.to_string())?;
        Ok(row.and_then(|r| r.hash))
    }

    async fn sent_since(&self, since: u64) -> Result<u64, String> {
        self.store
            .run(move |c| store::count_txs_since(c, "close", since))
            .await
            .map_err(|e| e.to_string())
    }

    async fn send(
        &self,
        nullifier: &str,
        proof: Vec<u8>,
        inputs: Vec<Vec<u8>>,
    ) -> Result<String, String> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let args = json!({
            "proof": b64.encode(&proof),
            "inputs": inputs.iter().map(|i| b64.encode(i)).collect::<Vec<_>>(),
        });
        let row = self
            .tx
            .submit(TxRequest {
                kind: "close",
                method: "mutual_close",
                args: args.to_string().into_bytes(),
                gas: CLOSE_GAS,
                idem_key: Some(nullifier.to_string()),
            })
            .await
            .map_err(|e| e.to_string())?;
        let hash = row.hash.clone().ok_or("the transaction has no hash")?;
        // Await FINAL and read the receipts in the background; the answer waits only a while.
        let (tx, id, alarms, nullifier) = (
            self.tx.clone(),
            row.id,
            self.alarms.clone(),
            nullifier.to_string(),
        );
        let driving = tokio::spawn(async move {
            let end = tx.drive(id).await;
            let failure = match &end {
                Err(e) => Some(e.to_string()),
                Ok(end) if !end.success => Some(end.detail.clone()),
                Ok(_) => None,
            };
            if let Some(why) = failure {
                alarms
                    .raise(
                        &format!("close_failed:{nullifier}"),
                        "An operator-sent close failed",
                        &format!("mutual_close for nullifier {nullifier} failed: {why}"),
                    )
                    .await;
            }
            end
        });
        match tokio::time::timeout(std::time::Duration::from_secs(ANSWER_WAIT_SECS), driving).await
        {
            Ok(Ok(Err(e))) => Err(e.to_string()),
            Ok(Ok(Ok(end))) if !end.success => Err(end.detail),
            _ => Ok(hash),
        }
    }
}

/// `(nullifier, proof, inputs, sent at)`.
type SentClose = (String, Vec<u8>, Vec<Vec<u8>>, u64);

/// Records what it is asked to send and answers with a made-up hash.
#[derive(Default)]
pub struct FakeCloseSender {
    sent: Mutex<Vec<SentClose>>,
    fail: Mutex<bool>,
    clock_secs: Mutex<Option<Arc<dyn Clock>>>,
}

impl FakeCloseSender {
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock_secs: Mutex::new(Some(clock)),
            ..Self::default()
        }
    }

    /// `(nullifier, proof, inputs)` of every send.
    pub fn sent(&self) -> Vec<(String, Vec<u8>, Vec<Vec<u8>>)> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(n, p, i, _)| (n.clone(), p.clone(), i.clone()))
            .collect()
    }

    pub fn set_failing(&self, fail: bool) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = fail;
    }

    fn now(&self) -> u64 {
        self.clock_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |c| c.now_secs())
    }
}

#[async_trait]
impl CloseSender for FakeCloseSender {
    async fn existing(&self, nullifier: &str) -> Result<Option<String>, String> {
        let now = self.now();
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        Ok(sent
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (n, _, _, at))| n == nullifier && now < at + crate::tx::IDEM_WINDOW_SECS)
            .map(|(i, _)| format!("fakehash{i}")))
    }

    async fn sent_since(&self, since: u64) -> Result<u64, String> {
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        Ok(sent.iter().filter(|(_, _, _, at)| *at >= since).count() as u64)
    }

    async fn send(
        &self,
        nullifier: &str,
        proof: Vec<u8>,
        inputs: Vec<Vec<u8>>,
    ) -> Result<String, String> {
        if *self.fail.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err("rpc down".into());
        }
        let at = self.now();
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        sent.push((nullifier.into(), proof, inputs, at));
        Ok(format!("fakehash{}", sent.len() - 1))
    }
}

/// One close on its way to the sender; counted against the daily limit until dropped.
struct PendingSend(Arc<Mutex<u64>>);

impl PendingSend {
    fn take(pending: &Arc<Mutex<u64>>, sent_today: u64) -> Option<Self> {
        let mut n = pending.lock().unwrap_or_else(|e| e.into_inner());
        if sent_today + *n >= SENDS_PER_DAY {
            return None;
        }
        *n += 1;
        Some(Self(pending.clone()))
    }
}

impl Drop for PendingSend {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
    }
}

pub struct Closer {
    vault: Arc<dyn VaultView>,
    verifier: Arc<dyn WithdrawalProofVerifier>,
    sender: Arc<dyn CloseSender>,
    clock: Arc<dyn Clock>,
    alarms: Arc<Alarms>,
    chain_id: u64,
    contract: Felt252,
    pool: Arc<Semaphore>,
    /// Start times (Unix seconds) of the verifications of the last minute.
    verifies: Mutex<VecDeque<u64>>,
    /// Closes past the daily check and not yet counted by the sender, so concurrent closes
    /// cannot all read 49.
    pending: Arc<Mutex<u64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseBody {
    proof: String,
    inputs: Vec<String>,
}

fn malformed(why: impl Into<String>) -> CloseError {
    CloseError::new(Code::MalformedRequest, why)
}

fn word_u64(word: &[u8; 32]) -> Option<u64> {
    let (high, low) = word.split_at(24);
    high.iter()
        .all(|b| *b == 0)
        .then(|| u64::from_be_bytes(low.try_into().expect("8 bytes")))
}

fn word_u128(word: &[u8; 32]) -> Option<u128> {
    let (high, low) = word.split_at(16);
    high.iter()
        .all(|b| *b == 0)
        .then(|| u128::from_be_bytes(low.try_into().expect("16 bytes")))
}

/// The 16 public input words as the statement they spell, `None` when a word is not canonical
/// for its field (a small number with bits set above its width).
fn statement(words: &[[u8; 32]]) -> Option<WithdrawalPublicInputsV2C> {
    let w = |i: usize| &words[i];
    let fields = w(8);
    if fields[..4].iter().any(|b| *b != 0) {
        return None;
    }
    if w(10)[..12].iter().any(|b| *b != 0) {
        return None;
    }
    Some(WithdrawalPublicInputsV2C {
        protocol_version: u16::try_from(word_u64(w(0))?).ok()?,
        chain_id: word_u64(w(1))?,
        contract_address: Felt252(*w(2)),
        note_leaf: Felt252(*w(3)),
        state_signing_key_x: Felt252(*w(4)),
        state_signing_key_y: Felt252(*w(5)),
        clearance_signing_key_x: Felt252(*w(6)),
        clearance_signing_key_y: Felt252(*w(7)),
        note_id: u32::from_be_bytes(fields[4..8].try_into().expect("4 bytes")),
        deposit_amount: u128::from_be_bytes(fields[8..24].try_into().expect("16 bytes")),
        expiry_ts: u64::from_be_bytes(fields[24..32].try_into().expect("8 bytes")),
        final_balance: word_u128(w(9))?,
        destination: w(10)[12..].try_into().expect("20 bytes"),
        withdrawal_nullifier: Felt252(*w(11)),
        kind: u8::try_from(word_u64(w(12))?).ok()?,
        next_nullifier: Felt252(*w(13)),
        conceded: word_u128(w(14))?,
        valid_until: word_u64(w(15))?,
    })
}

/// The proof bytes, the input words as sent, and the statement they spell.
type Decoded = (Vec<u8>, Vec<Vec<u8>>, WithdrawalPublicInputsV2C);

/// Strict decoding of the body: one JSON object with exactly `proof` and `inputs`, the proof 256
/// bytes of base64 and 16 inputs of 32 bytes each.
pub(crate) fn decode(body: &[u8]) -> Result<Decoded, CloseError> {
    let value = parse_strict(body).map_err(malformed)?;
    if !only_keys(&value, &["proof", "inputs"]) {
        return Err(malformed("unknown field"));
    }
    let parsed: CloseBody = serde_json::from_value(value).map_err(|e| malformed(e.to_string()))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let proof = b64
        .decode(&parsed.proof)
        .map_err(|_| malformed("proof is not base64"))?;
    if proof.len() != PROOF_BYTES {
        return Err(malformed("proof is not 256 bytes"));
    }
    if parsed.inputs.len() != INPUTS {
        return Err(malformed("a close has 16 public inputs"));
    }
    let mut inputs = Vec::with_capacity(INPUTS);
    let mut words = Vec::with_capacity(INPUTS);
    for item in &parsed.inputs {
        let bytes = b64
            .decode(item)
            .map_err(|_| malformed("an input is not base64"))?;
        words.push(
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| malformed("an input is not 32 bytes"))?,
        );
        inputs.push(bytes);
    }
    let public = statement(&words).ok_or_else(|| malformed("an input is not canonical"))?;
    Ok((proof, inputs, public))
}

impl Closer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vault: Arc<dyn VaultView>,
        verifier: Arc<dyn WithdrawalProofVerifier>,
        sender: Arc<dyn CloseSender>,
        clock: Arc<dyn Clock>,
        alarms: Arc<Alarms>,
        chain_id: u64,
        contract: Felt252,
    ) -> Self {
        Self {
            vault,
            verifier,
            sender,
            clock,
            alarms,
            chain_id,
            contract,
            pool: Arc::new(Semaphore::new(VERIFY_POOL)),
            verifies: Mutex::new(VecDeque::new()),
            pending: Arc::new(Mutex::new(0)),
        }
    }

    fn unreadable() -> CloseError {
        CloseError::new(Code::OperatorCapacity, "the vault could not be read").retry(5)
    }

    /// Records a verification, or refuses when the last minute already held the allowed number.
    fn rate_check(&self) -> Result<(), CloseError> {
        let now = self.clock.now_secs();
        let mut window = self.verifies.lock().unwrap_or_else(|e| e.into_inner());
        while window.front().is_some_and(|at| now >= at + 60) {
            window.pop_front();
        }
        if window.len() >= VERIFIES_PER_MINUTE {
            let wait = window
                .front()
                .map_or(1, |at| (at + 60).saturating_sub(now).max(1));
            return Err(
                CloseError::new(Code::CloseRateLimited, "too many closes this minute").retry(wait),
            );
        }
        window.push_back(now);
        Ok(())
    }

    async fn check_and_send(&self, body: &[u8]) -> Result<String, CloseError> {
        let (proof, inputs, public) = decode(body)?;
        let nullifier = felt_hex(&public.withdrawal_nullifier);

        // The public inputs name this deployment and this operator.
        let config = self.vault.config().await.map_err(|_| Self::unreadable())?;
        let wrong = |why: &str| CloseError::new(Code::ProofInvalid, why.to_string());
        if public.protocol_version != PROTOCOL_VERSION
            || public.chain_id != self.chain_id
            || public.contract_address != self.contract
        {
            return Err(wrong("public inputs name another deployment"));
        }
        if felt_hex(&public.state_signing_key_x) != config.operator_pk.x.to_ascii_lowercase()
            || felt_hex(&public.state_signing_key_y) != config.operator_pk.y.to_ascii_lowercase()
        {
            return Err(wrong("the state key is not the operator's"));
        }
        if felt_hex(&public.clearance_signing_key_x) != config.clearance_pk.x.to_ascii_lowercase()
            || felt_hex(&public.clearance_signing_key_y)
                != config.clearance_pk.y.to_ascii_lowercase()
        {
            return Err(wrong("the clearance key is not the operator's"));
        }
        if public.kind != 1 {
            return Err(wrong("not a mutual close"));
        }
        let now = self.clock.now_secs();
        if public.valid_until < now + MIN_VALID_SECS {
            return Err(CloseError::new(
                Code::ProofExpired,
                "the proof must stay valid for at least 10 minutes",
            ));
        }

        // A repeat gets the hash of the close already sent, whatever the vault says by now.
        if let Some(hash) = self
            .sender
            .existing(&nullifier)
            .await
            .map_err(|_| Self::unreadable())?
        {
            return Ok(hash);
        }

        // The note and its nullifier, read from the vault at a final block.
        let note = self
            .vault
            .note(public.note_id)
            .await
            .map_err(|_| Self::unreadable())?;
        match note.as_ref().map(|n| n.status.as_str()) {
            Some("Active") => {}
            Some("PendingExit") => {
                return Err(CloseError::new(
                    Code::NullifierInExit,
                    "an exit is pending for this note",
                ))
            }
            _ => {
                return Err(CloseError::new(
                    Code::NoteNotActive,
                    "the note is not active",
                ))
            }
        }
        if self
            .vault
            .nullifier_used(&nullifier)
            .await
            .map_err(|_| Self::unreadable())?
        {
            return Err(CloseError::new(
                Code::NullifierInExit,
                "the nullifier is spent: this state is in an exit or was left behind",
            ));
        }
        let exits = self.vault.exits().await.map_err(|_| Self::unreadable())?;
        if exits.iter().any(|e| e.note_id == public.note_id) {
            return Err(CloseError::new(
                Code::NullifierInExit,
                "an exit is pending for this note",
            ));
        }

        // Limits before the pairing.
        let day_start = now / SECS_PER_DAY * SECS_PER_DAY;
        let sent_today = self
            .sender
            .sent_since(day_start)
            .await
            .map_err(|_| Self::unreadable())?;
        let slot = PendingSend::take(&self.pending, sent_today);
        if slot.is_none() {
            return Err(CloseError::new(
                Code::CloseRateLimited,
                "the operator sends at most 50 closes a day",
            )
            .retry(day_start + SECS_PER_DAY - now));
        }
        let permit = self.pool.clone().try_acquire_owned().map_err(|_| {
            CloseError::new(Code::OperatorCapacity, "verification pool full").retry(1)
        })?;
        self.rate_check()?;
        let wire = Groth16ProofWire {
            backend: ProofBackendWire::Groth16Bn254,
            proof: base64::engine::general_purpose::STANDARD.encode(&proof),
        };
        let verifier = self.verifier.clone();
        let public_for_check = public.clone();
        let ok = tokio::task::spawn_blocking(move || {
            let ok = verifier.verify(&public_for_check, &wire);
            drop(permit);
            ok
        })
        .await
        .map_err(|_| CloseError::new(Code::ProofInvalid, "verification failed"))?;
        if !ok {
            return Err(CloseError::new(
                Code::ProofInvalid,
                "Groth16 verification failed",
            ));
        }

        let _slot = slot;
        let hash = self
            .sender
            .send(&nullifier, proof, inputs)
            .await
            .map_err(|_| {
                CloseError::new(
                    Code::ProviderError,
                    "the chain did not take the transaction",
                )
            })?;
        if sent_today + 1 >= SENDS_ALARM {
            self.alarms
                .raise(
                    "closes_per_day",
                    "Operator-sent closes are high today",
                    &format!(
                        "{} closes sent today; the limit is {SENDS_PER_DAY} a day.",
                        sent_today + 1
                    ),
                )
                .await;
        }
        Ok(hash)
    }
}

#[async_trait]
impl CloseApi for Closer {
    async fn close(&self, body: &[u8]) -> Result<String, CloseError> {
        if body.len() > CLOSE_BODY_LIMIT {
            return Err(CloseError::new(
                Code::BodyTooLarge,
                "request body over 64 KiB",
            ));
        }
        self.check_and_send(body).await
    }
}

#[derive(Serialize, Deserialize)]
struct Call {
    /// The raw request body, base64.
    body: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reply {
    Hash(String),
    Refused(CloseError),
}

/// Serves the closer on a Unix socket, one JSON line per call. A peer whose user id is not
/// `allowed_uid` is dropped before a byte is read.
pub async fn serve(listener: UnixListener, closer: Arc<dyn CloseApi>, allowed_uid: u32) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(allowed_uid) {
            continue;
        }
        let closer = closer.clone();
        tokio::spawn(async move {
            let _ = handle(stream, closer).await;
        });
    }
}

async fn handle(stream: UnixStream, closer: Arc<dyn CloseApi>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    // A peer that connects and says nothing must not hold the task.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        BufReader::new(read.take(2 * CLOSE_BODY_LIMIT as u64)).read_line(&mut line),
    )
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    let reply = match serde_json::from_str::<Call>(&line).ok().and_then(|c| {
        base64::engine::general_purpose::STANDARD
            .decode(c.body)
            .ok()
    }) {
        None => Reply::Refused(malformed("not a typed call")),
        Some(body) => match closer.close(&body).await {
            Ok(hash) => Reply::Hash(hash),
            Err(e) => Reply::Refused(e),
        },
    };
    let mut bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await
}

/// Calls a closer process over its Unix socket.
pub struct SocketCloser(pub PathBuf);

#[async_trait]
impl CloseApi for SocketCloser {
    async fn close(&self, body: &[u8]) -> Result<String, CloseError> {
        let unavailable =
            || CloseError::new(Code::OperatorCapacity, "the closer is not reachable").retry(5);
        let call = async {
            let stream = UnixStream::connect(&self.0)
                .await
                .map_err(|_| unavailable())?;
            let (read, mut write) = stream.into_split();
            let mut bytes = serde_json::to_vec(&Call {
                body: base64::engine::general_purpose::STANDARD.encode(body),
            })
            .expect("a call serialises");
            bytes.push(b'\n');
            write.write_all(&bytes).await.map_err(|_| unavailable())?;
            let mut line = String::new();
            BufReader::new(read.take(16 * 1024))
                .read_line(&mut line)
                .await
                .map_err(|_| unavailable())?;
            match serde_json::from_str::<Reply>(&line) {
                Ok(Reply::Hash(hash)) => Ok(hash),
                Ok(Reply::Refused(e)) => Err(e),
                Err(_) => Err(unavailable()),
            }
        };
        // The closer may wait on the chain for its own answer deadline.
        tokio::time::timeout(std::time::Duration::from_secs(ANSWER_WAIT_SECS + 15), call)
            .await
            .unwrap_or_else(|_| Err(unavailable()))
    }
}

/// Binds the closer's socket, readable and writable by its owner only.
pub fn bind_socket(path: &Path) -> std::io::Result<UnixListener> {
    crate::signer::bind_socket(path)
}

/// The answer body of `POST /v1/close`.
pub fn answer(hash: &str) -> Value {
    json!({ "tx_hash": hash })
}
