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
  init --rpc URL --operator URL --account ID [--socks HOST:PORT] [backup options | --no-backup]
  backup-setup [backup options]     sets up the encrypted backup
      backup options: --place DIR (the second place; repeat for more) | --local-only,
      and --backup-passphrase [--backup-passphrase-file FILE] to use a passphrase of 12 or
      more characters instead of a key pair (the key pair's private key is shown once)
  backup                            writes the backup now (over the owner socket)
  restore --from DIR_OR_FILE... [--backup-key-file FILE]
                                    into an empty --home, from the private key or passphrase
  restore-secret --note ID --secret-file FILE [--payout ACCOUNT]
                                    from a note secret alone: the note can be exited, not spent
  note-secret --note ID             shows a note's secret after a confirmation on the terminal
  serve                             the local gateway on 127.0.0.1:8788
  status
  verify-web [--socks HOST:PORT]    checks the web hosts against the signed manifest, over Tor and (after a yes) directly
  deposit --key-file FILE --amount BASE_UNITS [--payout ACCOUNT]
  recover --key-file FILE           finds notes again after a crash
  close --note ID [--delay SECS] [--family NAME] [--via-operator]
  exit --note ID [--family NAME]    files request_exit; finalize follows after the window
  finalize --note ID [--family NAME]
  claim-payout --note ID [--family NAME]
  submitter add --family NAME --account ACCOUNT | list | assign --note ID --family NAME
The last group talks to `nymirad serve` over the owner socket in <home>/run.";

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
            backup: cfg.backup_target(),
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
        Ok(g) => Arc::new(g.with_guard(cfg.guard())),
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
    // A signed policy in the state directory is used when it is newer than the one signed and
    // shipped with this build (a newer one, downloaded); an older or equal one never replaces it.
    // A file that exists but cannot be read is an error, not "no file".
    let file = match std::fs::read(home.join("attestation-policy.json")) {
        Ok(f) => Some(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return refuse(format!("attestation-policy.json: {e}")),
    };
    match nymirad::attest::Policy::choose(
        file.as_deref(),
        include_bytes!("../pins/attestation-policy.json"),
        &signer,
        MIN_POLICY_VERSION,
    ) {
        Ok(policy) if policy.deployments.is_empty() => {
            refuse("attestation-policy.json lists no deployment".into())
        }
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
            let mut cfg = Config {
                rpc_url: flag(&mut args, "--rpc").unwrap_or_else(|| die(USAGE)),
                operator_url: flag(&mut args, "--operator").unwrap_or_else(|| die(USAGE)),
                account: flag(&mut args, "--account").unwrap_or_else(|| die(USAGE)),
                socks: flag(&mut args, "--socks"),
                ..Config::default()
            };
            // Backups are on by default: the state is lost with the machine otherwise.
            if switch(&mut args, "--no-backup") {
                eprintln!("warning: no backup. If this machine is lost, an exit can be defeated and the note expire to the treasury.");
            } else {
                setup_backup(&mut cfg, &home, &mut args);
            }
            cfg.save(&home).unwrap_or_else(|e| die(e));
            println!("wrote {}", home.join("config.json").display());
        }
        "backup-setup" => {
            let mut cfg = Config::load(&home).unwrap_or_else(|e| die(e));
            setup_backup(&mut cfg, &home, &mut args);
            cfg.save(&home).unwrap_or_else(|e| die(e));
        }
        "backup" => owner_command(&home, "backup", &mut args).await,
        "restore" => restore_command(&home, &source, &mut args),
        "restore-secret" => restore_secret_command(&home, mode, &source, &mut args, clock).await,
        "note-secret" => {
            let note = note_id(&mut args);
            let passphrase = source.read().unwrap_or_else(|e| die(e));
            let notes = NoteStore::open(&home.join("notes.sealed"), &passphrase, ROUNDS)
                .unwrap_or_else(|e| die(e));
            let entry = notes
                .list()
                .unwrap_or_else(|e| die(e))
                .into_iter()
                .find(|n| n.note_id == Some(note))
                .unwrap_or_else(|| die(format!("no note {note}")));
            if !Terminal.confirm_text(&format!(
                "This shows the secret of note {note}. Whoever has it can exit the note, and spend it if they also have its state. Type yes to show it: "
            )) {
                die("not confirmed; nothing shown");
            }
            println!("{}", entry.secret);
        }
        "verify-web" => {
            let (key, origins) = nymirad::webverify::compiled().unwrap_or_else(|e| die(e));
            let cfg = match flag(&mut args, "--socks") {
                Some(socks) => Config {
                    socks: Some(socks),
                    ..Config::default()
                },
                None => Config::load(&home).unwrap_or_else(|e| die(e)),
            };
            let transport = connect(&cfg, &home, TOR_WAIT).await;
            let direct =
                Terminal.confirm_text(&format!("{}\n> ", nymirad::webverify::DIRECT_QUESTION));
            let plan = nymirad::webverify::Plan {
                key,
                profile: match mode {
                    Mode::Testnet => "testnet",
                    Mode::Mainnet => "mainnet",
                },
                primary: origins[0],
                mirror: origins[1],
            };
            let report = nymirad::webverify::verify(&plan, &transport, direct).await;
            print!("{}", report.render());
            if !report.ok() {
                std::process::exit(1);
            }
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
            for w in cfg.backup_warnings() {
                eprintln!("warning: {w}");
            }
            if !cfg.guard().enabled {
                eprintln!("warning: the expiry guard is off: nymirad will not stop leases near expiry, close, exit or remind you");
            }
            let gateway = build_gateway(&cfg, &home, mode, &source, clock.clone()).await;
            if let Some(gw) = &gateway {
                let listener = nymirad::ownersock::bind(&home).unwrap_or_else(|e| die(e));
                println!(
                    "owner socket: {}",
                    nymirad::ownersock::socket_path(&home).display()
                );
                tokio::spawn(nymirad::ownersock::serve(listener, gw.clone()));
            }
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
                for w in cfg.backup_warnings() {
                    println!("warning: {w}");
                }
                if cfg.backup_target().is_some() {
                    println!("{}", backup_line(&cfg, clock.now_secs()));
                }
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
                backup: cfg.backup_target(),
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
                    for w in &a.warnings {
                        eprintln!("warning: {w}");
                    }
                    println!("{}", a.report());
                }
                Err(e) => die(e),
            }
        }
        "close" | "exit" | "finalize" | "claim-payout" | "submitter" => {
            owner_command(&home, &command, &mut args).await
        }
        _ => die(USAGE),
    }
}

fn note_id(args: &mut Vec<String>) -> u32 {
    flag(args, "--note")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| die(USAGE))
}

/// Parses an owner command and sends it to the daemon.
async fn owner_command(home: &std::path::Path, command: &str, args: &mut Vec<String>) {
    use nymirad::ownersock::{request, Command};
    let family = flag(args, "--family");
    let cmd = match command {
        "close" => Command::Close {
            note: note_id(args),
            delay_secs: flag(args, "--delay").map(|d| {
                d.parse()
                    .unwrap_or_else(|_| die("--delay is a number of seconds"))
            }),
            family,
            via_operator: switch(args, "--via-operator"),
        },
        "exit" => Command::Exit {
            note: note_id(args),
            family,
        },
        "finalize" => Command::Finalize {
            note: note_id(args),
            family,
        },
        "claim-payout" => Command::ClaimPayout {
            note: note_id(args),
            family,
        },
        "backup" => Command::Backup,
        _ => match (args.first().map(String::as_str), family) {
            (Some("add"), Some(family)) => {
                args.remove(0);
                Command::SubmitterAdd {
                    family,
                    account: flag(args, "--account").unwrap_or_else(|| die(USAGE)),
                }
            }
            (Some("assign"), Some(family)) => {
                args.remove(0);
                Command::SubmitterAssign {
                    note: note_id(args),
                    family,
                }
            }
            (Some("list"), _) => Command::SubmitterList,
            _ => die(USAGE),
        },
    };
    let result = request(
        home,
        &cmd,
        |line| println!("{line}"),
        |question| Terminal.confirm_text(&format!("{question}\n> ")),
    )
    .await;
    match result {
        Ok(text) => println!("{text}"),
        Err(e) => die(e),
    }
}

/// What a backup place holds and when it was last written, for `status`.
fn backup_line(cfg: &Config, now_secs: u64) -> String {
    let mut parts = Vec::new();
    for dir in &cfg.backup_places {
        let state = dir.join(backup::STATE_FILE);
        let written = std::fs::metadata(&state)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        parts.push(match written {
            Some(t) => format!(
                "{} (state backup {} min ago)",
                dir.display(),
                now_secs.saturating_sub(t) / 60
            ),
            None => format!("{} (no state backup yet)", dir.display()),
        });
    }
    format!("Backup: on, {}", parts.join("; "))
}

/// A secret from a 0600 file or, without one, from the terminal.
fn secret_input(file: Option<String>, prompt: &str) -> zeroize::Zeroizing<String> {
    match file {
        Some(path) => PassphraseSource::File(path.into())
            .read()
            .unwrap_or_else(|e| die(e)),
        None => {
            zeroize::Zeroizing::new(rpassword::prompt_password(prompt).unwrap_or_else(|e| die(e)))
        }
    }
}

/// Makes the backup key (or derives it from a passphrase) and records where the copies go: the
/// local one under the state directory and the second place the user names.
fn setup_backup(cfg: &mut Config, home: &std::path::Path, args: &mut Vec<String>) {
    let local_only = switch(args, "--local-only");
    let mut places = vec![home.join("backup")];
    while let Some(place) = flag(args, "--place") {
        places.push(place.into());
    }
    if places.len() < 2 && !local_only {
        die("name a second place for the backup copy with --place DIR (another disk, a synced folder, a USB stick), or pass --local-only to keep it on this machine only");
    }
    let wants_passphrase = switch(args, "--backup-passphrase");
    let pass_file = flag(args, "--backup-passphrase-file");
    if wants_passphrase || pass_file.is_some() {
        let pass = secret_input(pass_file, "Backup passphrase (12 or more characters): ");
        let (kdf, public) = backup::passphrase_setup(&pass).unwrap_or_else(|e| die(e));
        cfg.backup_recipient = Some(public);
        cfg.backup_kdf = Some(kdf);
        println!(
            "Backup key derived from your passphrase with Argon2id ({} MiB, {} passes, {} lane, random salt kept in each backup). The passphrase is not stored: without it the backup cannot be opened, and nymirad cannot recover it.",
            backup::KDF_M_KIB / 1024,
            backup::KDF_T,
            backup::KDF_P
        );
    } else {
        let (private, public) = backup::generate_identity();
        cfg.backup_recipient = Some(public.clone());
        cfg.backup_kdf = None;
        println!("Backup public key: {public}");
        println!(
            "Backup PRIVATE key (shown once; keep it off this machine, not in this machine's keychain and not beside the backup):\n{}",
            private.as_str()
        );
    }
    println!(
        "Backups go to: {}",
        places
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    cfg.backup_places = places;
}

/// `restore`: opens the backup with the key or passphrase the user holds and writes the state
/// into an empty state directory. Runs in this process, after a confirmation on the terminal.
fn restore_command(home: &std::path::Path, source: &PassphraseSource, args: &mut Vec<String>) {
    let mut from: Vec<PathBuf> = Vec::new();
    while let Some(f) = flag(args, "--from") {
        from.push(f.into());
    }
    if from.is_empty() {
        die(USAGE);
    }
    let secret = secret_input(
        flag(args, "--backup-key-file"),
        "Backup private key or passphrase: ",
    );
    let state = backup::load(&from, &secret).unwrap_or_else(|e| die(e));
    println!(
        "The backup holds {} note(s), taken at Unix time {}.",
        state.notes.len(),
        state.taken_at
    );
    if !Terminal.confirm_text(&format!(
        "Restore it into {}, which must be empty. The notes and wallets will be sealed under the passphrase you enter next. Type yes: ",
        home.display()
    )) {
        die("not confirmed; nothing written");
    }
    let passphrase = source.read().unwrap_or_else(|e| die(e));
    let restored =
        backup::restore_into(home, state, &passphrase, ROUNDS).unwrap_or_else(|e| die(e));
    for (id, status, has_wallet) in &restored.notes {
        println!(
            "Note {}: {:?}{}",
            id.map_or("?".to_string(), |i| i.to_string()),
            status,
            if *has_wallet {
                ""
            } else {
                " (no wallet state: it restarts from its deposit)"
            }
        );
    }
    println!(
        "{}",
        backup::restore_notice(&restored, SystemClock.now_secs())
    );
}

/// `restore-secret`: one note from its secret alone, stored so it can only be exited.
async fn restore_secret_command(
    home: &std::path::Path,
    mode: Mode,
    source: &PassphraseSource,
    args: &mut Vec<String>,
    clock: Arc<dyn Clock>,
) {
    let cfg = Config::load(home)
        .unwrap_or_else(|e| die(format!("{e} (run `nymirad init ... --no-backup` first)")));
    let note = note_id(args);
    let secret = secret_input(flag(args, "--secret-file"), "Note secret: ");
    let payout = flag(args, "--payout").unwrap_or_else(|| cfg.account.clone());
    println!("{}", backup::SECRET_ONLY_NOTICE);
    println!("Payout account: {payout}. It must be the account named at deposit; the vault refuses an exit to another one.");
    if !Terminal.confirm_text("Type yes to continue: ") {
        die("not confirmed; nothing written");
    }
    let passphrase = source.read().unwrap_or_else(|e| die(e));
    let transport = connect(&cfg, home, TOR_WAIT).await;
    let rpc = Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e));
    let pinned = pins::compiled(mode);
    #[cfg(feature = "mock")]
    let pinned = pinned.or_else(|| pins::demo(home));
    let vault = pinned
        .map(|p| p.vault)
        .unwrap_or_else(|| die("no vault is pinned in this build"));
    let entry = nymirad::restore::from_secret(
        home,
        &rpc,
        &vault,
        mode.chain_label(),
        &secret,
        note,
        &payout,
        &passphrase,
        ROUNDS,
        clock.now_secs(),
    )
    .await
    .unwrap_or_else(|e| die(e));
    println!(
        "Note {} is stored for exit. Next: `nymirad submitter add --family personal --account ACCOUNT` (while `nymirad serve` runs), then `nymirad exit --note {}`.",
        entry.note_id.unwrap_or(note),
        entry.note_id.unwrap_or(note)
    );
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
