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
use nymira_operator::operator::{felt_hex, Config, Operator, Parts};
use nymira_operator::provider::FakeProvider;
use nymira_operator::replica::FakeReplica;
use nymira_operator::store::Store;
use nymira_operator::verify::{GrothVerifier, ProofVerifier};
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

fn inputs_from(bundle: &Bundle) -> RequestPublicInputsV2 {
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
    }
}

pub struct Rig {
    pub op: Arc<Operator>,
    pub clock: Arc<ManualClock>,
    pub vault: Arc<FakeVault>,
    pub provider: Arc<FakeProvider>,
    pub replica: Arc<FakeReplica>,
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
    pub credit: u128,
    pub tune: Box<dyn FnOnce(&mut Config)>,
    pub clock_secs: u64,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            verifier: Verifier::Stub(Arc::new(Counting::default())),
            credit: 100_000_000,
            tune: Box::new(|_| {}),
            clock_secs: T0,
        }
    }
}

async fn launch(
    dir: &std::path::Path,
    clock: &Arc<ManualClock>,
    vault: &Arc<FakeVault>,
    provider: &Arc<FakeProvider>,
    replica: &Arc<FakeReplica>,
    setup: Setup,
) -> (Arc<Operator>, String, tokio::task::JoinHandle<()>) {
    let mut cfg = config();
    (setup.tune)(&mut cfg);
    let verifier: Arc<dyn ProofVerifier> = match setup.verifier {
        Verifier::Real => Arc::new(GrothVerifier::load(std::path::Path::new(SETUP_DIR)).unwrap()),
        Verifier::Stub(v) => v,
    };
    let store = Store::open(&dir.join("operator.db")).unwrap();
    let op = Operator::start(Parts {
        cfg,
        clock: clock.clone(),
        vault: vault.clone(),
        provider: provider.clone(),
        replica: replica.clone(),
        verifier,
        store,
        state_signer: CompactSigner::from_seed(&Felt252::from_u64(1)),
    })
    .await
    .expect("operator starts");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = nymira_operator::http::router(op.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (op, url, server)
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
        let provider = Arc::new(FakeProvider::new(clock.clone(), setup.credit));
        let replica = Arc::new(FakeReplica::default());
        let (op, url, server) =
            launch(dir.path(), &clock, &vault, &provider, &replica, setup).await;
        Self {
            op,
            clock,
            vault,
            provider,
            replica,
            url,
            http: reqwest::Client::new(),
            dir,
            server,
        }
    }

    /// A new operator process over the same database, vault, provider and replica.
    pub async fn restart(&mut self, setup: Setup) {
        self.server.abort();
        let (op, url, server) = launch(
            self.dir.path(),
            &self.clock,
            &self.vault,
            &self.provider,
            &self.replica,
            setup,
        )
        .await;
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
