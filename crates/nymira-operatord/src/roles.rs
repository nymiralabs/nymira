//! The five processes. Each builds only what it needs and loads only its own secrets:
//!
//! | role    | NEAR key                                   | other secrets                         |
//! |---------|--------------------------------------------|---------------------------------------|
//! | api     | none                                       | none (lease keys arrive in memory)    |
//! | minter  | none                                       | NEAR AI login, usage key, seal passphrase |
//! | signer  | `heartbeat`                                | state and clearance passphrases       |
//! | closer  | `mutual_close`                             | none                                  |
//! | watcher | `upkeep` and the separate exit-answer key  | the log's entry-signing key           |

use std::sync::Arc;

use ed25519_dalek::{SigningKey, VerifyingKey};
use nymira_near_codec::{chain_id, contract_address, Network};
use nymira_operator::alarm::{
    AlarmSink, Alarms, EmailSink, SmtpConfig, SmtpSecurity, TelegramSink,
};
use nymira_operator::chain::{RpcVault, VaultView};
use nymira_operator::clock::{Clock, SystemClock};
use nymira_operator::closer::{self, Closer, NearCloseSender};
use nymira_operator::minter_service::{self, RemoteProvider};
use nymira_operator::nearai::{LeaseChat, NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::operator::{Config as OperatorConfig, Operator, Parts, Profile};
use nymira_operator::replica::{NoReplica, Replica};
use nymira_operator::replica_http::HttpLink;
use nymira_operator::replica_socket::{self, SocketReplica};
use nymira_operator::replicator::Replicator;
use nymira_operator::replog::Log;
use nymira_operator::sealed::SealedFile;
use nymira_operator::secretfile;
use nymira_operator::serve::{self, Limits};
use nymira_operator::signer::{self, Signer, SocketSigner, SocketStateSigner};
use nymira_operator::store::Store;
use nymira_operator::tx::{NearHeartbeat, NearTx};
use nymira_operator::usage::Minter;
use nymira_operator::verify::{GrothVerifier, GrothWithdrawalVerifier};
use nymira_operator::watcher::{NearCalls, Watcher, WatcherConfig};
use zkapi_types::Felt252;

use crate::config::Config;

type Failure = String;

fn euid() -> u32 {
    // SAFETY: no arguments, no failure mode.
    unsafe { libc::geteuid() }
}

fn network(cfg: &Config) -> Network {
    if cfg.network == "mainnet" {
        Network::Mainnet
    } else {
        Network::Testnet
    }
}

fn store(cfg: &Config) -> Result<Store, Failure> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
    Store::open(&cfg.db()).map_err(|e| format!("database: {e}"))
}

/// A 32-byte seed from a 0400 file holding 64 hex digits.
pub fn seed(cfg: &Config, name: &str) -> Result<[u8; 32], Failure> {
    let bytes = secretfile::read(&cfg.secret(name)).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{name} is not text"))?;
    hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{name} is not 64 hex digits"))
}

fn near_key(cfg: &Config, name: &str) -> Result<SigningKey, Failure> {
    Ok(SigningKey::from_bytes(&seed(cfg, name)?))
}

fn text_secret(path: &std::path::Path) -> Result<String, Failure> {
    let bytes = secretfile::read(path).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn alarms(cfg: &Config, clock: Arc<dyn Clock>) -> Result<Arc<Alarms>, Failure> {
    let mut sinks: Vec<Arc<dyn AlarmSink>> = Vec::new();
    if let Some(t) = &cfg.alarms.telegram {
        sinks.push(Arc::new(TelegramSink::new(
            "https://api.telegram.org",
            &text_secret(&t.token_file)?,
            &t.chat_id,
        )));
    }
    if let Some(e) = &cfg.alarms.email {
        let password = e.password_file.as_deref().map(text_secret).transpose()?;
        let smtp = SmtpConfig {
            host: e.host.clone(),
            port: e.port,
            security: match e.security.as_str() {
                "tls" => SmtpSecurity::Tls,
                "starttls" => SmtpSecurity::StartTls,
                other => return Err(format!("email security is tls or starttls, not {other}")),
            },
            username: e.username.clone(),
            password,
            from: e.from.clone(),
            to: e.to.clone(),
        };
        sinks.push(Arc::new(EmailSink::new(&smtp)?));
    }
    Ok(Arc::new(Alarms::new(sinks, clock)))
}

fn vault(cfg: &Config, rpc: &str) -> Arc<dyn VaultView> {
    Arc::new(RpcVault::new(rpc, cfg.vault_account.clone()))
}

fn tx(
    cfg: &Config,
    signer_id: &str,
    key: SigningKey,
    store: Store,
    clock: Arc<dyn Clock>,
) -> Arc<NearTx> {
    Arc::new(NearTx::new(
        cfg.rpc_urls[0].clone(),
        signer_id,
        cfg.vault_account.clone(),
        key,
        store,
        clock,
    ))
}

fn bind(cfg: &Config, name: &str) -> Result<tokio::net::UnixListener, Failure> {
    std::fs::create_dir_all(&cfg.run_dir).map_err(|e| e.to_string())?;
    nymira_operator::ipc::bind(&cfg.socket(name)).map_err(|e| format!("{name} socket: {e}"))
}

fn never<T>() -> T {
    unreachable!("a server loop does not return")
}

/// The public API: no secret, no NEAR key. Its one TCP listener is the only one of the service.
pub async fn api(cfg: &Config) -> Result<(), Failure> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = store(cfg)?;
    let net = network(cfg);
    let contract = Felt252(contract_address(&cfg.vault_account));
    let mut oc = OperatorConfig::new(&cfg.vault_account, chain_id(net), contract, &cfg.web_origin);
    oc.profile = match cfg.profile.as_str() {
        "mainnet" => Profile::Mainnet,
        "testnet" => Profile::Testnet,
        _ => Profile::Sandbox,
    };
    oc.host = cfg.host.clone();
    if cfg.budget == "hackathon" {
        oc.budget = nymira_operator::budget::BudgetConfig::hackathon();
        oc.credit = nymira_operator::budget::CreditConfig::hackathon();
    }
    if let Some(path) = &cfg.team_note_ids_file {
        oc.team_note_ids = nymira_operator::operator::load_team_note_ids(path)?;
    }

    let http = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let remote = Arc::new(RemoteProvider::new(
        &cfg.socket("minter"),
        LeaseChat::new(http, &cfg.near_ai.base_url),
    ));
    let replica = Arc::new(SocketReplica::new(&cfg.socket("replica")));
    replica
        .refresh()
        .await
        .map_err(|e| format!("the replication owner is not up: {}", e.0))?;
    let state_signer = SocketStateSigner::connect(&cfg.socket("signer"))
        .await
        .map_err(|e| format!("the signer is not up: {e}"))?;

    let alarms = alarms(cfg, clock.clone())?;
    let op = Operator::start(Parts {
        cfg: oc,
        clock,
        vault: vault(cfg, &cfg.rpc_urls[0]),
        provider: remote.clone(),
        replica,
        verifier: Arc::new(GrothVerifier::load(&cfg.setup_dir)?),
        withdrawal_verifier: Arc::new(GrothWithdrawalVerifier::load(&cfg.setup_dir)?),
        closer: Arc::new(closer::SocketCloser(cfg.socket("closer"))),
        store,
        state_signer: Arc::new(state_signer),
        signer: Arc::new(SocketSigner(cfg.socket("signer"))),
        minter: remote,
        alarms,
    })
    .await
    .map_err(|e| e.to_string())?;
    op.spawn_background();

    match serve::raise_fd_limit(serve::FD_LIMIT) {
        Ok(limit) if limit >= serve::FD_LIMIT => {}
        Ok(limit) => eprintln!("nymira-operatord: file descriptor limit is {limit}"),
        Err(e) => eprintln!("nymira-operatord: could not raise the descriptor limit: {e}"),
    }
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("listen {}: {e}", cfg.listen))?;
    serve::serve(
        listener,
        nymira_operator::http::router(op),
        Limits::default(),
    )
    .await;
    never()
}

/// The key minter: the NEAR AI login and the usage key live here and nowhere else.
pub async fn minter(cfg: &Config) -> Result<(), Failure> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let alarms = alarms(cfg, clock.clone())?;
    let passphrase = secretfile::passphrase("seal passphrase").map_err(|e| e.to_string())?;
    let open = |name: &str| {
        SealedFile::open(&cfg.data_dir.join(name), &passphrase, signer::KDF_ROUNDS)
            .map_err(|e| e.to_string())
    };
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
    let first = cfg
        .near_ai
        .first_refresh_token_file
        .as_deref()
        .map(text_secret)
        .transpose()?;
    let adapter = NearAiAdapter::new(
        NearAiConfig {
            user_agent: BROWSER_USER_AGENT.into(),
            base_url: cfg.near_ai.base_url.clone(),
            org_id: cfg.near_ai.org_id.clone(),
            workspace_id: cfg.near_ai.workspace_id.clone(),
        },
        clock.clone(),
        alarms,
        open("nearai-login.sealed")?,
        open("nearai-keys.sealed")?,
        first.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    let usage_key = Minter::from_seed(&seed(cfg, "usage")?);
    if hex::encode(usage_key.public_key().to_bytes()) != cfg.usage_public_key {
        return Err("the usage key does not match usage_public_key".into());
    }
    minter_service::serve(
        bind(cfg, "minter")?,
        Arc::new(adapter),
        usage_key,
        clock,
        euid(),
    )
    .await;
    never()
}

/// The signer: the state and clearance keys from two passphrases typed at start-up, and the
/// `heartbeat` key.
pub async fn signer(cfg: &Config) -> Result<(), Failure> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = store(cfg)?;
    let state = secretfile::passphrase("state key passphrase").map_err(|e| e.to_string())?;
    let clearance =
        secretfile::passphrase("clearance key passphrase").map_err(|e| e.to_string())?;
    let (state_seed, clearance_seed) =
        signer::seeds_from_passphrases(&state, &clearance, signer::KDF_ROUNDS)
            .map_err(|e| e.to_string())?;
    let (state_key, clearance_key) =
        Signer::signers_from_seeds(&state_seed, &clearance_seed, cfg.sandbox())
            .map_err(|e| e.to_string())?;
    let usage_pk: [u8; 32] = hex::decode(&cfg.usage_public_key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("usage_public_key is not 64 hex digits")?;
    let heartbeat_tx = tx(
        cfg,
        &cfg.operator_account,
        near_key(cfg, "heartbeat")?,
        store.clone(),
        clock.clone(),
    );
    // A heartbeat signed before a crash and never sent goes out now.
    heartbeat_tx.resume().await;
    let heartbeat = NearHeartbeat(heartbeat_tx);
    let replica = Arc::new(SocketReplica::new(&cfg.socket("replica")));
    let signer = Arc::new(
        Signer::new(
            state_key,
            clearance_key,
            store,
            vault(cfg, &cfg.rpc_urls[0]),
            VerifyingKey::from_bytes(&usage_pk).map_err(|e| e.to_string())?,
            chain_id(network(cfg)),
            Felt252(contract_address(&cfg.vault_account)),
            clock,
            Arc::new(heartbeat),
        )
        .map_err(|e| e.to_string())?
        .with_status_host(&cfg.host)
        .with_replica(replica),
    );
    signer.spawn_background();
    signer::serve(bind(cfg, "signer")?, signer, euid()).await;
    never()
}

/// The closer: only the `mutual_close` key.
pub async fn closer(cfg: &Config) -> Result<(), Failure> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = store(cfg)?;
    let alarms = alarms(cfg, clock.clone())?;
    let close_tx = tx(
        cfg,
        &cfg.operator_account,
        near_key(cfg, "mutual_close")?,
        store.clone(),
        clock.clone(),
    );
    // A close signed before a crash and never sent goes out now.
    close_tx.resume().await;
    let sender = NearCloseSender {
        tx: close_tx,
        store,
        clock: clock.clone(),
        alarms: alarms.clone(),
    };
    let closer = Closer::new(
        vault(cfg, &cfg.rpc_urls[0]),
        Arc::new(GrothWithdrawalVerifier::load(&cfg.setup_dir)?),
        Arc::new(sender),
        clock,
        alarms,
        chain_id(network(cfg)),
        Felt252(contract_address(&cfg.vault_account)),
    );
    closer::serve(
        closer::bind_socket(&cfg.socket("closer")).map_err(|e| e.to_string())?,
        Arc::new(closer),
        euid(),
    )
    .await;
    never()
}

/// The watcher, which also owns the replication log: the API and the signer reach the replica
/// through its socket.
pub async fn watcher(cfg: &Config) -> Result<(), Failure> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = store(cfg)?;
    let alarms = alarms(cfg, clock.clone())?;
    let replica: Arc<dyn Replica> = match &cfg.replica {
        Some(r) => {
            let pk: [u8; 32] = hex::decode(&r.public_key)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or("the replica public_key is not 64 hex digits")?;
            let link = HttpLink::new(&r.url).map_err(|e| e.to_string())?;
            Arc::new(
                Replicator::new(Log::new(store.clone()), Arc::new(link), pk, clock.clone())
                    .with_signing_key(near_key(cfg, "entry")?),
            )
        }
        None => Arc::new(NoReplica),
    };
    tokio::spawn(replica_socket::serve(
        bind(cfg, "replica")?,
        replica,
        euid(),
    ));

    let providers: Vec<Arc<dyn VaultView>> = cfg.rpc_urls.iter().map(|u| vault(cfg, u)).collect();
    let calls = NearCalls {
        answer: tx(
            cfg,
            &cfg.answer_account,
            near_key(cfg, "answer")?,
            store.clone(),
            clock.clone(),
        ),
        upkeep: tx(
            cfg,
            &cfg.operator_account,
            near_key(cfg, "upkeep")?,
            store.clone(),
            clock.clone(),
        ),
    };
    // Calls signed before a crash and never sent go out now.
    calls.answer.resume().await;
    calls.upkeep.resume().await;
    let watcher = Arc::new(
        Watcher::new(
            providers,
            Arc::new(calls),
            store,
            clock,
            alarms,
            WatcherConfig::default(),
        )?
        .with_id("laptop"),
    );
    watcher.spawn_background();
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// The replica node on the second machine: the log the laptop pushes to, with the operator's
/// entry key pinned and retention run every hour.
pub async fn replica(cfg: &Config) -> Result<(), Failure> {
    let section = cfg
        .replica_node
        .as_ref()
        .ok_or("this config has no replica_node section")?;
    let pk: [u8; 32] = hex::decode(&section.entry_public_key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("entry_public_key is not 64 hex digits")?;
    let node = Arc::new(
        nymira_operator::replog::ReplicaNode::new(Log::new(store(cfg)?), near_key(cfg, "replica")?)
            .with_laptop_key(pk),
    );
    let purging = node.clone();
    tokio::spawn(async move {
        loop {
            let now = SystemClock.now_secs();
            let _ = purging.purge(now).await;
            tokio::time::sleep(std::time::Duration::from_secs(
                nymira_operator::operator::PURGE_EVERY_SECS,
            ))
            .await;
        }
    });
    let listener = tokio::net::TcpListener::bind(&section.listen)
        .await
        .map_err(|e| format!("listen {}: {e}", section.listen))?;
    serve::serve(
        listener,
        nymira_operator::replica_http::router(node),
        Limits::default(),
    )
    .await;
    never()
}
