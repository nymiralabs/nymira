//! Test harness: a running operator over HTTP with a scripted vault, the fake provider and a
//! manual clock. Real proofs come from the shared proof generator; most tests reuse one real
//! proof's curve points with fresh nullifiers behind a verifier stub, and the tests that matter
//! for soundness run the real Groth16 verifier.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use nymira_near_codec::{chain_id, contract_address, Network};
use nymira_operator::chain::{FakeVault, Key, VaultConfig};
use nymira_operator::clock::ManualClock;
use nymira_operator::closer::{CloseApi, CloseError, Closer, FakeCloseSender};
use nymira_operator::operator::{felt_hex, Config, Operator, Parts};
use nymira_operator::provider::FakeProvider;
use nymira_operator::replica::FakeReplica;
use nymira_operator::signer::{FakeHeartbeat, LocalSigner, Signer};
use nymira_operator::store::Store;
use nymira_operator::usage::Minter;
use nymira_operator::verify::{
    GrothVerifier, GrothWithdrawalVerifier, ProofVerifier, WithdrawalProofVerifier,
};
use nymira_testkit::proofs::{signing_key, Bundle, ProofGen};
use serde_json::{json, Value};
use zkapi_core::MerkleTree;
use zkapi_proof::compact::CompactSigner;
use zkapi_proof::dev::{tree_with, DevNote, DevRequest};
use zkapi_types::wire::{
    ApiRequestV2, Groth16ProofWire, OpenRouterLeaseAuthorization, ProofBackendWire,
};
use zkapi_types::{
    canonical_payload_hash, canonical_request_context, Felt252, RequestPublicInputsV2,
};

pub const VAULT: &str = "vault.nymira.testnet";
pub const ORIGIN: &str = "https://nymira.pages.dev";
/// The manual clock starts here (seconds); well after the unix epoch, well before the note expires.
pub const T0: u64 = 1_900_000_000;
pub const EXPIRY: u64 = 4_000_000_000;
pub const SETUP_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/zkapi/protocol/setup/v2-concession-dev"
);

pub fn network() -> Network {
    Network::Testnet
}

pub fn gen() -> &'static ProofGen {
    static GEN: OnceLock<ProofGen> = OnceLock::new();
    GEN.get_or_init(|| ProofGen::new(network(), VAULT).unwrap())
}

pub fn contract() -> Felt252 {
    Felt252(contract_address(VAULT))
}

pub fn note() -> DevNote {
    DevNote {
        secret: Felt252::from_u64(42),
        note_id: 0,
        deposit_amount: 2_000_000,
        expiry: EXPIRY,
    }
}

pub fn tree() -> MerkleTree {
    tree_with(&[&note()])
}

pub fn root_hex() -> String {
    felt_hex(&tree().root())
}

pub const REAL_CLIENT_REQUEST_ID: &str = "real-request";

pub fn payload() -> String {
    serde_json::to_string(&OpenRouterLeaseAuthorization::default()).unwrap()
}

fn felt(word: &[u8; 32]) -> Felt252 {
    Felt252(*word)
}

pub fn inputs_from(bundle: &Bundle) -> RequestPublicInputsV2 {
    let w = &bundle.inputs_wire;
    RequestPublicInputsV2 {
        protocol_version: felt(&w[0]).to_u64().unwrap() as u16,
        chain_id: felt(&w[1]).to_u64().unwrap(),
        contract_address: felt(&w[2]),
        active_root: felt(&w[3]),
        state_signing_key_x: felt(&w[4]),
        state_signing_key_y: felt(&w[5]),
        request_time: felt(&w[6]).to_u64().unwrap(),
        solvency_bound: felt(&w[7]).to_u128().unwrap(),
        request_nullifier: felt(&w[8]),
        authorization_tag: felt(&w[9]),
        anonymous_commitment_x: felt(&w[10]),
        anonymous_commitment_y: felt(&w[11]),
    }
}

/// One genuine request proof (note 0, bound 0.10, time `T0`), proved once per test process.
pub fn real_request() -> &'static ApiRequestV2 {
    static REQUEST: OnceLock<ApiRequestV2> = OnceLock::new();
    REQUEST.get_or_init(|| {
        let payload = payload();
        let payload_hash = canonical_payload_hash(payload.as_bytes());
        let state = note().genesis(Felt252::from_u64(7));
        let bundle = gen()
            .request(
                &state,
                &tree(),
                &DevRequest {
                    request_time: T0,
                    solvency_bound: 100_000,
                    request_context: canonical_request_context(
                        REAL_CLIENT_REQUEST_ID,
                        &payload_hash,
                    ),
                    rerandomization: Felt252::from_u64(9),
                },
            )
            .unwrap();
        ApiRequestV2 {
            client_request_id: REAL_CLIENT_REQUEST_ID.into(),
            payload,
            payload_hash,
            public_inputs: inputs_from(&bundle),
            proof: Groth16ProofWire {
                backend: ProofBackendWire::Groth16Bn254,
                proof: bundle.proof_base64(),
            },
        }
    })
}

/// A request that reuses the real proof's curve points with its own nullifier, id, bound and
/// time. Only a verifier stub accepts it.
pub fn variant(n: u64, bound: u128, time: u64, root: &str) -> ApiRequestV2 {
    let mut r = real_request().clone();
    r.client_request_id = format!("req-{n}");
    r.public_inputs.request_nullifier = Felt252::from_u64(1_000 + n);
    r.public_inputs.solvency_bound = bound;
    r.public_inputs.request_time = time;
    r.public_inputs.active_root = Felt252::from_hex(root).unwrap();
    r
}

pub fn body(r: &ApiRequestV2) -> Vec<u8> {
    serde_json::to_vec(r).unwrap()
}

/// Accepts every proof and counts the calls.
#[derive(Default)]
pub struct Counting(pub AtomicUsize);

impl ProofVerifier for Counting {
    fn verify(&self, _: &RequestPublicInputsV2, _: &Groth16ProofWire) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// Blocks every verification until opened.
#[derive(Default)]
pub struct Gate {
    state: Mutex<(usize, bool)>,
    cv: Condvar,
}

impl Gate {
    pub fn entered(&self) -> usize {
        self.state.lock().unwrap().0
    }

    pub fn open(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

impl ProofVerifier for Gate {
    fn verify(&self, _: &RequestPublicInputsV2, _: &Groth16ProofWire) -> bool {
        let mut s = self.state.lock().unwrap();
        s.0 += 1;
        while !s.1 {
            s = self.cv.wait(s).unwrap();
        }
        true
    }
}

/// Accepts every withdrawal proof.
pub struct AcceptWithdrawals;

impl WithdrawalProofVerifier for AcceptWithdrawals {
    fn verify(&self, _: &zkapi_types::WithdrawalPublicInputsV2C, _: &Groth16ProofWire) -> bool {
        true
    }
}

/// A closer that is not there.
pub struct NoCloser;

#[async_trait::async_trait]
impl CloseApi for NoCloser {
    async fn close(&self, _: &[u8]) -> Result<String, CloseError> {
        Err(CloseError::new(
            nymira_operator::error::Code::OperatorCapacity,
            "no closer",
        ))
    }
}

pub enum Verifier {
    Real,
    Stub(Arc<dyn ProofVerifier>),
}

pub fn vault_config() -> VaultConfig {
    let key = |seed| {
        let pk = signing_key(seed);
        Key {
            x: felt_hex(&pk.x),
            y: felt_hex(&pk.y),
        }
    };
    VaultConfig {
        operator_pk: key(1),
        clearance_pk: key(2),
        chain_id: u128::from(chain_id(network())),
        note_fee: 10_000,
        close_fee: 10_000,
        bound_classes: vec![100_000, 500_000],
        charge_step: 1_000,
        min_charge: 2_000,
        slot_rate: 1_000,
        exit_delay_secs: 7 * 86_400,
    }
}

pub struct Rig {
    pub op: Arc<Operator>,
    pub clock: Arc<ManualClock>,
    pub vault: Arc<FakeVault>,
    pub provider: Arc<FakeProvider>,
    pub replica: Arc<FakeReplica>,
    pub signer: Arc<Signer>,
    pub heartbeat: Arc<FakeHeartbeat>,
    pub sink: Arc<nymira_operator::alarm::FakeSink>,
    pub sender: Arc<FakeCloseSender>,
    pub minter: Minter,
    pub url: String,
    pub http: reqwest::Client,
    pub dir: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub struct Setup {
    pub verifier: Verifier,
    /// `None`: accept every withdrawal proof, or the real check when `verifier` is `Real`.
    pub withdrawal: Option<Arc<dyn WithdrawalProofVerifier>>,
    pub credit: u128,
    pub tune: Box<dyn FnOnce(&mut Config)>,
    pub clock_secs: u64,
    /// Builds the replica from the operator's own store: tests that run the real replicator.
    pub replica: Option<Box<dyn FnOnce(Store) -> Arc<dyn nymira_operator::replica::Replica>>>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            verifier: Verifier::Stub(Arc::new(Counting::default())),
            withdrawal: None,
            credit: 100_000_000,
            tune: Box::new(|_| {}),
            clock_secs: T0,
            replica: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn launch(
    dir: &std::path::Path,
    clock: &Arc<ManualClock>,
    vault: &Arc<FakeVault>,
    provider: &Arc<FakeProvider>,
    replica: &Arc<FakeReplica>,
    minter: &Minter,
    heartbeat: &Arc<FakeHeartbeat>,
    sink: &Arc<nymira_operator::alarm::FakeSink>,
    sender: &Arc<FakeCloseSender>,
    setup: Setup,
) -> (
    Arc<Operator>,
    Arc<Signer>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let mut cfg = config();
    (setup.tune)(&mut cfg);
    let real = matches!(setup.verifier, Verifier::Real);
    let verifier: Arc<dyn ProofVerifier> = match setup.verifier {
        Verifier::Real => Arc::new(GrothVerifier::load(std::path::Path::new(SETUP_DIR)).unwrap()),
        Verifier::Stub(v) => v,
    };
    let withdrawal: Arc<dyn WithdrawalProofVerifier> = match setup.withdrawal {
        Some(v) => v,
        None if real => {
            Arc::new(GrothWithdrawalVerifier::load(std::path::Path::new(SETUP_DIR)).unwrap())
        }
        None => Arc::new(AcceptWithdrawals),
    };
    let alarms = Arc::new(nymira_operator::alarm::Alarms::new(
        vec![sink.clone()],
        clock.clone(),
    ));
    let closer = Arc::new(Closer::new(
        vault.clone(),
        withdrawal.clone(),
        sender.clone(),
        clock.clone(),
        alarms.clone(),
        chain_id(network()),
        contract(),
    ));
    let store = Store::open(&dir.join("operator.db")).unwrap();
    let replica_dyn: Arc<dyn nymira_operator::replica::Replica> = match setup.replica {
        Some(build) => build(store.clone()),
        None => replica.clone(),
    };
    // The signer has its own handle on the database, as a separate process would.
    let signer = Arc::new(
        Signer::new(
            CompactSigner::from_seed(&Felt252::from_u64(1)),
            CompactSigner::from_seed(&Felt252::from_u64(2)),
            Store::open(&dir.join("operator.db")).unwrap(),
            vault.clone(),
            minter.public_key(),
            chain_id(network()),
            contract(),
            clock.clone(),
            heartbeat.clone(),
        )
        .unwrap()
        .with_replica(replica_dyn.clone()),
    );
    let op = Operator::start(Parts {
        cfg,
        clock: clock.clone(),
        vault: vault.clone(),
        provider: provider.clone(),
        replica: replica_dyn,
        verifier,
        withdrawal_verifier: withdrawal,
        closer,
        store,
        state_signer: CompactSigner::from_seed(&Felt252::from_u64(1)),
        signer: Arc::new(LocalSigner(signer.clone())),
        minter: minter.clone(),
        alarms,
    })
    .await
    .expect("operator starts");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = nymira_operator::http::router(op.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (op, signer, url, server)
}

pub fn config() -> Config {
    let mut cfg = Config::new(VAULT, chain_id(network()), contract(), ORIGIN);
    cfg.host = "operator.test".into();
    cfg
}

impl Rig {
    pub async fn new(setup: Setup) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(setup.clock_secs * 1000));
        let vault = Arc::new(FakeVault::new(vault_config(), &root_hex()));
        // Every test proof names note 0.
        vault.edit(|v| {
            v.notes.insert(0, "Active".into());
        });
        let provider = Arc::new(FakeProvider::new(clock.clone(), setup.credit));
        let replica = Arc::new(FakeReplica::default());
        let minter = Minter::generate();
        let heartbeat = Arc::new(FakeHeartbeat::default());
        let sink = Arc::new(nymira_operator::alarm::FakeSink::default());
        let sender = Arc::new(FakeCloseSender::with_clock(clock.clone()));
        let (op, signer, url, server) = launch(
            dir.path(),
            &clock,
            &vault,
            &provider,
            &replica,
            &minter,
            &heartbeat,
            &sink,
            &sender,
            setup,
        )
        .await;
        Self {
            op,
            clock,
            vault,
            provider,
            replica,
            signer,
            heartbeat,
            sink,
            sender,
            minter,
            url,
            http: reqwest::Client::new(),
            dir,
            server,
        }
    }

    /// A new operator process over the same database, vault, provider and replica.
    pub async fn restart(&mut self, setup: Setup) {
        self.server.abort();
        let (op, signer, url, server) = launch(
            self.dir.path(),
            &self.clock,
            &self.vault,
            &self.provider,
            &self.replica,
            &self.minter,
            &self.heartbeat,
            &self.sink,
            &self.sender,
            setup,
        )
        .await;
        self.signer = signer;
        self.op = op;
        self.url = url;
        self.server = server;
    }

    pub async fn post_lease(&self, body: Vec<u8>) -> (u16, Value, reqwest::header::HeaderMap) {
        self.post_lease_with(body, None).await
    }

    pub async fn post_lease_with(
        &self,
        body: Vec<u8>,
        puzzle: Option<&str>,
    ) -> (u16, Value, reqwest::header::HeaderMap) {
        let mut request = self
            .http
            .post(format!("{}/v2/openrouter/leases", self.url))
            .header("content-type", "application/json")
            .body(body);
        if let Some(p) = puzzle {
            request = request.header("X-Nymira-Puzzle", p);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let value = response.json().await.unwrap_or(Value::Null);
        (status, value, headers)
    }

    pub async fn lease(&self, r: &ApiRequestV2) -> (u16, Value, reqwest::header::HeaderMap) {
        self.post_lease(body(r)).await
    }

    pub async fn status(&self) -> Value {
        self.http
            .get(format!("{}/v1/status", self.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// A fresh stub-accepted request valid now, at the current root.
    pub fn fresh(&self, n: u64) -> ApiRequestV2 {
        variant(n, 100_000, self.clock_now(), &root_hex())
    }

    pub fn clock_now(&self) -> u64 {
        use nymira_operator::clock::Clock;
        self.clock.now_secs()
    }
}

pub fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

pub fn json_error(status: u16, v: &Value, expected_status: u16, expected_code: &str) {
    assert_eq!(status, expected_status, "{v}");
    assert_eq!(code(v), expected_code, "{v}");
    assert!(v["error"]["message"].is_string());
    let _ = json!(null);
}

// ---------------------------------------------------------------- withdrawal proofs

use zkapi_types::wire::{ClearanceOwnershipProofV2C, ClearanceRequestV2C};
use zkapi_types::{WithdrawalPublicInputsV2C, PROTOCOL_VERSION};

/// The statement of a kind-2 or kind-1 withdrawal for nullifier `1000 + n`, naming this
/// deployment and its two keys. Only a verifier stub accepts a proof for it.
pub fn withdrawal_statement(n: u64, kind: u8, valid_until: u64) -> WithdrawalPublicInputsV2C {
    let (state, clearance) = (signing_key(1), signing_key(2));
    WithdrawalPublicInputsV2C {
        protocol_version: PROTOCOL_VERSION,
        chain_id: chain_id(network()),
        contract_address: contract(),
        note_leaf: Felt252::from_u64(5),
        state_signing_key_x: state.x,
        state_signing_key_y: state.y,
        clearance_signing_key_x: clearance.x,
        clearance_signing_key_y: clearance.y,
        note_id: 0,
        deposit_amount: 2_000_000,
        expiry_ts: EXPIRY,
        final_balance: 1_990_000,
        destination: if kind == 2 { [0; 20] } else { [7; 20] },
        withdrawal_nullifier: Felt252::from_u64(1_000 + n),
        kind,
        next_nullifier: Felt252::ZERO,
        conceded: 0,
        valid_until,
    }
}

pub fn dummy_proof() -> Groth16ProofWire {
    Groth16ProofWire {
        backend: ProofBackendWire::Groth16Bn254,
        proof: base64_of(&[0u8; 256]),
    }
}

pub fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The JSON body of `POST /v2/withdraw/clearance`.
pub fn clearance_body(public: &WithdrawalPublicInputsV2C, proof: Groth16ProofWire) -> Vec<u8> {
    serde_json::to_vec(&ClearanceRequestV2C {
        withdrawal_nullifier: public.withdrawal_nullifier,
        ownership_proof: ClearanceOwnershipProofV2C {
            public_inputs: public.clone(),
            proof,
        },
    })
    .unwrap()
}

/// The statement of a real withdrawal proof, read back from its wire words.
pub fn statement_of(bundle: &Bundle) -> WithdrawalPublicInputsV2C {
    let w = |i: usize| Felt252(bundle.inputs_wire[i]);
    let fields = &bundle.inputs_wire[8];
    WithdrawalPublicInputsV2C {
        protocol_version: w(0).to_u64().unwrap() as u16,
        chain_id: w(1).to_u64().unwrap(),
        contract_address: w(2),
        note_leaf: w(3),
        state_signing_key_x: w(4),
        state_signing_key_y: w(5),
        clearance_signing_key_x: w(6),
        clearance_signing_key_y: w(7),
        note_id: u32::from_be_bytes(fields[4..8].try_into().unwrap()),
        deposit_amount: u128::from_be_bytes(fields[8..24].try_into().unwrap()),
        expiry_ts: u64::from_be_bytes(fields[24..32].try_into().unwrap()),
        final_balance: w(9).to_u128().unwrap(),
        destination: bundle.inputs_wire[10][12..].try_into().unwrap(),
        withdrawal_nullifier: w(11),
        kind: w(12).to_u64().unwrap() as u8,
        next_nullifier: w(13),
        conceded: w(14).to_u128().unwrap(),
        valid_until: w(15).to_u64().unwrap(),
    }
}

pub fn wire_proof(bundle: &Bundle) -> Groth16ProofWire {
    Groth16ProofWire {
        backend: ProofBackendWire::Groth16Bn254,
        proof: bundle.proof_base64(),
    }
}

/// The JSON body of `POST /v1/close` from statement words and a proof.
pub fn close_body(words: &[[u8; 32]], proof: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "proof": base64_of(proof),
        "inputs": words.iter().map(|w| base64_of(w)).collect::<Vec<_>>(),
    }))
    .unwrap()
}

/// The body of a close for a hand-made statement; only a verifier stub accepts it.
pub fn stub_close_body(public: &WithdrawalPublicInputsV2C) -> Vec<u8> {
    let words: Vec<[u8; 32]> = public
        .to_field_elements()
        .iter()
        .map(|f| *f.as_bytes())
        .collect();
    close_body(&words, &[0u8; 256])
}

impl Rig {
    pub async fn post_json(&self, path: &str, body: Vec<u8>) -> (u16, Value) {
        let response = self
            .http
            .post(format!("{}{path}", self.url))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    pub async fn get_json(&self, path: &str) -> (u16, Value) {
        let response = self
            .http
            .get(format!("{}{path}", self.url))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    pub async fn clearance(&self, public: &WithdrawalPublicInputsV2C) -> (u16, Value) {
        self.post_json(
            "/v2/withdraw/clearance",
            clearance_body(public, dummy_proof()),
        )
        .await
    }

    /// An ownership statement for nullifier `1000 + n`, valid for 3,000 seconds from now.
    pub fn ownership(&self, n: u64) -> WithdrawalPublicInputsV2C {
        withdrawal_statement(n, 2, self.clock_now() + 3_000)
    }
}

/// The spelling of nullifier `1000 + n`, the one `variant(n, ..)` and `withdrawal_statement(n, ..)` use.
pub fn nullifier_hex(n: u64) -> String {
    felt_hex(&Felt252::from_u64(1_000 + n))
}
