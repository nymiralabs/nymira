//! The `nymirad` command line. Secrets enter through the terminal, stdin or a 0600 file; the
//! funding key is read from a 0600 file in this process only.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use nymira_operator::clock::{Clock, SystemClock};
use nymira_operator::store::Store;
use nymira_operator::tx::NearTx;
use nymirad::config::Config;
use nymirad::deposit::{Confirm, Ctx, Summary};
use nymirad::gateway::{Gateway, GatewayConfig};
use nymirad::mode::Mode;
use nymirad::pins::{self, Tripwire};
use nymirad::quotes::{Board, OneClick, QuoteSource, BASE_URL};
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{NoteStore, PassphraseSource, ROUNDS};
use nymirad::token::OwnerToken;
use nymirad::tor::Tor;
use nymirad::{backup, guard, hardening, status};

const USAGE: &str =
    "usage: nymirad [--home DIR] [--passphrase-file FILE | --passphrase-stdin] <command>
commands:
  init --rpc URL --operator URL --account ID [--socks HOST:PORT]
  backup-setup [--place DIR]...     makes a key pair, shows the private key once
  serve                             the local gateway on 127.0.0.1:8788
  status
  deposit --key-file FILE --amount BASE_UNITS [--payout ACCOUNT]
  recover --key-file FILE           finds notes again after a crash";

struct Terminal;

impl Confirm for Terminal {
    fn confirm(&self, s: &Summary) -> bool {
        println!(
            "Deposit {} (+ fee {}) = {} from {}\nPayout account {} (fingerprint {})",
            s.denomination, s.note_fee, s.total, s.sender, s.payout, s.payout_fingerprint
        );
        Terminal.confirm_text("Type yes to sign: ")
    }
}

fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("nymirad: {message}");
    std::process::exit(1)
}

fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.remove(at);
    (at < args.len()).then(|| args.remove(at))
}

fn switch(args: &mut Vec<String>, name: &str) -> bool {
    args.iter()
        .position(|a| a == name)
        .map(|at| args.remove(at))
        .is_some()
}

fn funding_key(path: &str) -> SigningKey {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .unwrap_or_else(|e| die(format!("key file: {e}")))
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        die(format!("key file is readable by others (mode {mode:o})"));
    }
    let text = zeroize::Zeroizing::new(
        std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("key file: {e}"))),
    );
    let seed: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| die("key file must hold a 32-byte ed25519 seed in hex"));
    SigningKey::from_bytes(&seed)
}

/// Where this process's traffic goes: a SOCKS proxy the user runs, the Tor `nymirad` starts, or,
/// in demo builds only, nothing. Waits `wait` for a managed Tor to finish its bootstrap.
async fn connect(cfg: &Config, home: &std::path::Path, wait: std::time::Duration) -> Transport {
    if let Some(addr) = &cfg.socks {
        // Only a proxy on this machine: anything else would carry the destination names and
        // the identities to another host.
        match addr.parse::<std::net::SocketAddr>() {
            Ok(a) if a.ip().is_loopback() => {}
            _ => die(format!(
                "socks must be a loopback IP and port, like 127.0.0.1:9050, not {addr}"
            )),
        }
        return Transport::Socks(addr.clone());
    }
    if cfg.direct_allowed() {
        return Transport::Direct;
    }
    let tor_cfg = cfg.tor_config(home).unwrap_or_else(|e| die(e));
    let tor = Tor::start(&tor_cfg).await.unwrap_or_else(|e| die(e));
    eprintln!("{}", tor.status_line());
    match tor.wait_ready(wait).await {
        Ok(()) => eprintln!("{}", tor.status_line()),
        // The gateway stays closed (no request goes out) while the state is not Ready.
        Err(e) => eprintln!("warning: {e}"),
    }
    Transport::Tor(Arc::new(tor))
}

const TOR_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// The chat gateway, or `None` (with the reason on stderr) when something it needs is missing.
/// The vault pins are checked here as well as before every lease.
async fn build_gateway(
    cfg: &Config,
    home: &std::path::Path,
    mode: Mode,
    source: &PassphraseSource,
    clock: Arc<dyn Clock>,
) -> Option<Arc<Gateway>> {
    if cfg.rpc_url.is_empty() {
        return None;
    }
    let transport = connect(cfg, home, TOR_WAIT).await;
    let rpc = Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e));
    let pinned = pins::compiled(mode);
    #[cfg(feature = "mock")]
    let pinned = pinned.or_else(|| pins::demo(home));
    let Some(pinned) = pinned else {
        eprintln!("warning: no vault is pinned for {mode:?} in this build: chat is off");
        return None;
    };
    match Tripwire::default().check_against(&pinned, &rpc).await {
        Ok(()) => println!("vault pins match"),
        Err(diffs) => {
            for line in pins::warnings(&diffs) {
                eprintln!("warning: {line}");
            }
        }
    }
    let passphrase = source.read().unwrap_or_else(|e| die(e));
    let notes =
        NoteStore::open(&home.join("notes.sealed"), &passphrase, ROUNDS).unwrap_or_else(|e| die(e));
    let setup_dir = cfg.setup_dir.clone().or_else(|| {
        eprintln!("warning: config.json has no setup_dir (the proving key): chat is off");
        None
    })?;
    let proving_key_sha256 = compiled_proving_key_hash(cfg);
    let network = match mode {
        Mode::Testnet => nymira_near_codec::Network::Testnet,
        Mode::Mainnet => nymira_near_codec::Network::Mainnet,
    };
    let attestor = attestor(home);
    let gateway = Gateway::new(
        GatewayConfig {
            home: home.to_path_buf(),
            operator_url: cfg.operator_url.clone(),
            gateway_url: cfg
                .gateway_url
                .clone()
                .unwrap_or_else(|| nymirad::config::DEFAULT_GATEWAY.into()),
            vault: pinned.vault.clone(),
            network,
            setup_dir,
            proving_key_sha256,
            models: nymira_operator::proxy::ProxyConfig::default().models,
            rounds: ROUNDS,
            idle_secs: cfg.idle_secs.unwrap_or(120),
            share_pct: 90,
            prewarm_max_secs: 5,
            backup: cfg.backup_recipient.as_deref().and_then(|r| {
                backup::parse_recipient(r)
                    .ok()
                    .map(|r| (r, cfg.backup_places.clone()))
            }),
        },
        transport,
        rpc,
        pinned,
        notes,
        &passphrase,
        attestor,
        clock,
    );
    let gateway = match gateway {
        Ok(g) => Arc::new(g),
        Err(e) => {
            eprintln!("warning: chat is off: {e}");
            return None;
        }
    };
    for line in gateway.check_models().await {
        eprintln!("warning: model list: {line}");
    }
    gateway.spawn_background(std::time::Duration::from_secs(5));
    Some(gateway)
}

/// The proving key's SHA-256 pinned in this build. Demo builds may take it from `config.json`.
fn compiled_proving_key_hash(cfg: &Config) -> String {
    let _ = cfg;
    include_str!("../pins/proving-key.sha256")
        .trim()
        .to_string()
}

#[cfg(feature = "mock")]
fn attestor(home: &std::path::Path) -> Arc<dyn nymirad::attest::Attestor> {
    // Demo builds check the fake gateway's test evidence against a policy signed by the demo key.
    let signer: [u8; 32] = std::fs::read_to_string(home.join("policy-signer.pub"))
        .ok()
        .and_then(|t| hex::decode(t.trim()).ok())
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| die("demo build: policy-signer.pub is missing"));
    let json = std::fs::read(home.join("attestation-policy.json"))
        .unwrap_or_else(|e| die(format!("attestation-policy.json: {e}")));
    let policy = nymirad::attest::Policy::load(&json, &signer, MIN_POLICY_VERSION)
        .unwrap_or_else(|e| die(e));
    Arc::new(nymirad::attest::TestEvidence { policy })
}

/// NEAR AI's Intel and NVIDIA evidence, checked against the signed `attestation-policy.json` in
/// the state directory. Without a pinned policy signer, a readable policy that verifies and is
/// new enough, chat is refused rather than sent to a key nobody checked.
#[cfg(not(feature = "mock"))]
fn attestor(home: &std::path::Path) -> Arc<dyn nymirad::attest::Attestor> {
    let refuse = |why: String| -> Arc<dyn nymirad::attest::Attestor> {
        eprintln!("warning: {why}; chat requests will be refused");
        Arc::new(nymirad::attest::Unavailable)
    };
    let signer: Option<[u8; 32]> = hex::decode(include_str!("../pins/policy-signer.pub").trim())
        .ok()
        .and_then(|b| b.try_into().ok());
    let Some(signer) = signer else {
        return refuse("this build pins no attestation policy signer".into());
    };
    let json = match std::fs::read(home.join("attestation-policy.json")) {
        Ok(j) => j,
        Err(e) => return refuse(format!("attestation-policy.json: {e}")),
    };
    match nymirad::attest::Policy::load(&json, &signer, MIN_POLICY_VERSION) {
        Ok(policy) => Arc::new(nymirad::nearai::NearAiAttestor::new(policy)),
        Err(e) => refuse(format!("attestation-policy.json: {e}")),
    }
}

/// Policies older than this are refused.
const MIN_POLICY_VERSION: u32 = 1;

#[tokio::main]
async fn main() {
    if let Err(e) = hardening::apply() {
        die(format!("hardening: {e}"));
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let home = flag(&mut args, "--home")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| die("no HOME"))).join(".nymira")
        });
    let source = match (
        flag(&mut args, "--passphrase-file"),
        switch(&mut args, "--passphrase-stdin"),
    ) {
        (Some(path), _) => PassphraseSource::File(path.into()),
        (None, true) => PassphraseSource::Stdin,
        _ => PassphraseSource::Terminal,
    };
    let mode = Mode::from_env().unwrap_or_else(|e| die(e));
    let Some(command) = (!args.is_empty()).then(|| args.remove(0)) else {
        die(USAGE)
    };
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    match command.as_str() {
        "init" => {
            let cfg = Config {
                rpc_url: flag(&mut args, "--rpc").unwrap_or_else(|| die(USAGE)),
                operator_url: flag(&mut args, "--operator").unwrap_or_else(|| die(USAGE)),
                account: flag(&mut args, "--account").unwrap_or_else(|| die(USAGE)),
                socks: flag(&mut args, "--socks"),
                ..Config::default()
            };
            cfg.save(&home).unwrap_or_else(|e| die(e));
            println!("wrote {}", home.join("config.json").display());
        }
        "backup-setup" => {
            let mut cfg = Config::load(&home).unwrap_or_else(|e| die(e));
            while let Some(place) = flag(&mut args, "--place") {
                cfg.backup_places.push(place.into());
            }
            let (private, public) = backup::generate_identity();
            cfg.backup_recipient = Some(public.clone());
            cfg.save(&home).unwrap_or_else(|e| die(e));
            println!("Backup public key: {public}");
            println!(
                "Backup PRIVATE key (shown once, keep it off this machine):\n{}",
                private.as_str()
            );
        }
        "serve" => {
            let token =
                OwnerToken::load_or_create(&home.join("owner.token")).unwrap_or_else(|e| die(e));
            println!(
                "nymirad listening on 127.0.0.1:{}; owner token in {}",
                guard::PORT,
                home.join("owner.token").display()
            );
            let cfg = Config::load(&home).unwrap_or_else(|e| {
                eprintln!("warning: {e} (run init): chat is off");
                Config::default()
            });
            let gateway = build_gateway(&cfg, &home, mode, &source, clock.clone()).await;
            guard::serve(Arc::new(token), gateway)
                .await
                .unwrap_or_else(|e| die(e));
        }
        "status" | "deposit" | "recover" => {
            let cfg = Config::load(&home).unwrap_or_else(|e| die(e));
            let passphrase = source.read().unwrap_or_else(|e| die(e));
            let notes = NoteStore::open(&home.join("notes.sealed"), &passphrase, ROUNDS)
                .unwrap_or_else(|e| die(e));
            let transport = connect(&cfg, &home, TOR_WAIT).await;
            let tripwire = Arc::new(Tripwire::default());
            let rpc = Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e));
            let pinned = pins::compiled(mode);
            #[cfg(feature = "mock")]
            let pinned = pinned.or_else(|| pins::demo(&home));
            if command == "status" {
                let _ = match &pinned {
                    Some(p) => tripwire.check_against(p, &rpc).await,
                    None => tripwire.check(mode, &rpc, "").await,
                };
                let operator = status::operator_status(&cfg.operator_url, &transport).await;
                let quote_source: Option<Arc<dyn QuoteSource>> = match &transport {
                    Transport::Direct => None,
                    t => OneClick::new(BASE_URL, t)
                        .ok()
                        .map(|q| Arc::new(q) as Arc<dyn QuoteSource>),
                };
                let board = Board::new(mode, quote_source, clock.clone());
                println!(
                    "{}",
                    status::render(
                        &transport.status_line(),
                        operator.as_ref(),
                        &notes.list().unwrap_or_else(|e| die(e)),
                        &tripwire,
                        &board.view().await,
                        clock.now_secs()
                    )
                );
                return;
            }
            let pins =
                pinned.unwrap_or_else(|| die("no vault is pinned in this build; deposits are off"));
            for r in nymirad::records::Records::new(&home)
                .list()
                .iter()
                .rev()
                .take(5)
            {
                println!("{}", status::lease_line(r));
            }
            let key = funding_key(&flag(&mut args, "--key-file").unwrap_or_else(|| die(USAGE)));
            let tx_store = Store::open(&home.join("tx.db")).unwrap_or_else(|e| die(e));
            let token = rpc
                .view(&pins.vault, "config", serde_json::json!({}))
                .await
                .ok()
                .and_then(|c| c["token"].as_str().map(String::from))
                .unwrap_or_else(|| die("the vault could not be read"));
            let ctx = Ctx {
                mode,
                rpc: rpc.clone(),
                pins,
                tripwire,
                notes,
                tx: NearTx::new(
                    cfg.rpc_url.clone(),
                    cfg.account.clone(),
                    token,
                    key,
                    tx_store.clone(),
                    clock.clone(),
                )
                .with_deposit(1),
                tx_store,
                sender: cfg.account.clone(),
                clock,
                backup: cfg.backup_recipient.as_deref().and_then(|r| {
                    backup::parse_recipient(r)
                        .ok()
                        .map(|r| (r, cfg.backup_places.clone()))
                }),
                calendar_dir: home.join("calendar"),
            };
            if command == "recover" {
                for (commitment, result) in ctx.recover().await.unwrap_or_else(|e| die(e)) {
                    match result {
                        Ok(a) => println!("{commitment}: note {} accepted", a.note_id),
                        Err(e) => println!("{commitment}: {e}"),
                    }
                }
                return;
            }
            let amount: u128 = flag(&mut args, "--amount")
                .and_then(|a| a.parse().ok())
                .unwrap_or_else(|| die(USAGE));
            let payout = flag(&mut args, "--payout");
            match ctx.deposit(amount, payout.as_deref(), &Terminal).await {
                Ok(a) => {
                    println!(
                        "Note {} accepted. Expires at Unix time {}.",
                        a.note_id, a.expiry_secs
                    );
                    println!("Calendar file: {}", a.calendar.display());
                    for w in &a.warnings {
                        eprintln!("warning: {w}");
                    }
                    println!("{}", nymirad::deposit::WAIT_HINT);
                }
                Err(e) => die(e),
            }
        }
        _ => die(USAGE),
    }
}

impl Terminal {
    fn confirm_text(&self, prompt: &str) -> bool {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let read = match std::fs::File::open("/dev/tty") {
            Ok(file) => std::io::BufReader::new(file).read_line(&mut line).is_ok(),
            // Only a demo build may confirm on stdin, so scripts can run the walk.
            #[cfg(feature = "mock")]
            Err(_) => std::io::stdin().lock().read_line(&mut line).is_ok(),
            #[cfg(not(feature = "mock"))]
            Err(_) => false,
        };
        read && line.trim() == "yes"
    }
}
