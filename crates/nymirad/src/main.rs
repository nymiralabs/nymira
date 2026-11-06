//! The `nymirad` command line. Secrets enter through the terminal, stdin or a 0600 file; the
//! funding key is read from a 0600 file in this process only.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use nymira_operator::clock::{Clock, SystemClock};
use nymira_operator::store::Store;
use nymirad::agents::{self, Registry};
use nymirad::config::Config;
use nymirad::deposit::{Confirm, Ctx, Summary};
use nymirad::gateway::{Gateway, GatewayConfig};
use nymirad::mode::{IntentsPath, Mode};
use nymirad::pins::{self, Tripwire};
use nymirad::quotes::{Board, OneClick, QuoteSource, BASE_URL};
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{NoteStore, PassphraseSource, ROUNDS};
use nymirad::token::OwnerToken;
use nymirad::tor::Tor;
use nymirad::{backup, guard, hardening, status};

/// The terms of use, as of this build.
const TERMS: &str = include_str!("../../../TERMS.md");

const USAGE: &str =
    "usage: nymirad [--home DIR] [--passphrase-file FILE | --passphrase-stdin] <command>
commands:
  init --rpc URL --operator URL --account ID [--rpc-second URL] [--socks HOST:PORT] [--setup-dir DIR]
       [backup options | --no-backup]
      --setup-dir: the directory with the ceremony's request.pk and withdrawal.pk; both are
      checked against the hashes pinned in this build, and chat and close stay off without it.
      Get both files from the `final` release of github.com/nymiralabs/nymira-ceremony
  backup-setup [backup options]     sets up the encrypted backup
      backup options: --place DIR (the second place; repeat for more) | --local-only,
      and --backup-passphrase [--backup-passphrase-file FILE] to use a passphrase of 12 or
      more characters instead of a key pair (the key pair's private key is shown once)
  backup                            writes the backup now (over the owner socket)
  restore --from DIR_OR_FILE... [--backup-key-file FILE] [--policy-seen NAME=VERSION]...
                                    into an empty --home, from the private key or passphrase
  restore-secret --note ID --secret-file FILE [--payout ACCOUNT]
                                    from a note secret alone: the note can be exited, not spent
  note-secret --note ID             shows a note's secret after a confirmation on the terminal
  serve [--nras-direct]             the local gateway on 127.0.0.1:8788
      --nras-direct: send the GPU evidence straight to NVIDIA instead of through the operator's
      relay. NVIDIA then sees your IP address; there is no fall back from one to the other
  status
  terms                             prints the terms of use (the same file as TERMS.md in the repository)
  print-pins --network testnet|mainnet --vault ACCOUNT [--rpc URL]
                                    maintainers: reads the vault over public RPC (directly, no Tor)
                                    and prints the pins for crates/nymirad/pins/<network>.json;
                                    the proving-key hashes are in pins/proving-key.sha256 and
                                    pins/withdrawal-key.sha256 and are not printed
  verify-web [--socks HOST:PORT]    checks the web hosts against the signed manifest, over Tor and (after a yes) directly
  deposit --key-file FILE --amount BASE_UNITS [--payout ACCOUNT]
  deposit --source list|ethereum-usdc|base-usdc --amount BASE_UNITS --payout ACCOUNT --refund SOURCE_WALLET [--backup-key-file FILE]
  agent create --name NAME --transport unix|tcp --key-file FILE --amount BASE_UNITS
               [--agent-user USER [--group GROUP] --token-file PATH | --token-file PATH]
      a budget note for one agent: funded like any deposit, paid out to your own account,
      with a nyma_ token. unix: the agent runs as USER and uses the agent socket; the token
      file is made 0640 in GROUP. tcp: loopback port 8788, directly or through an SSH forward
  agent disable --name NAME         the next request on its token gets 401 token_disabled
  agent ack-warning --name NAME     clears a charge warning (the operator charged the note too much), so the agent can send requests again
  agent list
  agent policy --name NAME --file SIGNED_POLICY.json
      loads a policy signed with your NEAR account (NEP-413, full-access key): checks the
      signature and the key over several RPC endpoints, shows a summary and asks you to
      confirm; it applies from the agent's next request
  agent show                        each agent's spend today (UTC), its note's balance and expiry, and the policy in force
  agent revoke --name NAME [--delay SECS] [--family NAME] [--via-operator]
      takes the budget note back without the agent: disables its token, retires its lease,
      asks for a clearance and closes the note; files an exit only if the operator gives none
  agent approvals                   the requests held for your approval: ID, model, size, estimated cost
  agent approve --file SIGNED_APPROVAL.json
      hands the running nymirad an approval you signed on your own device (NEP-413)
  recover --intents COMMITMENT       resumes a saved funding attempt after a new direct-access confirmation
  recover --key-file FILE           finds notes again after a crash
  close --note ID [--delay SECS] [--family NAME] [--via-operator]
  exit --note ID [--family NAME]    files request_exit; finalize follows after the window
  finalize --note ID [--family NAME]
  claim-payout --note ID [--family NAME]
  claim-payout --intents COMMITMENT [--family NAME]  claims the saved payout account even when no note ID exists
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
    nras_direct: bool,
) -> Option<Arc<Gateway>> {
    if cfg.rpc_url.is_empty() {
        return None;
    }
    let transport = connect(cfg, home, TOR_WAIT).await;
    let rpc = Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e));
    let second = second_rpc(cfg, &transport);
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
    let withdrawal_key_sha256 = compiled_withdrawal_key_hash(cfg);
    let network = match mode {
        Mode::Testnet => nymira_near_codec::Network::Testnet,
        Mode::Mainnet => nymira_near_codec::Network::Mainnet,
    };
    let attestor = attestor(home, &cfg.operator_url, nras_direct);
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
            withdrawal_key_sha256,
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
        Ok(g) => Arc::new(g.with_guard(cfg.guard()).with_second_rpc(second)),
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

fn notes_have_accepted(notes: &NoteStore) -> bool {
    notes.list().is_ok_and(|n| {
        n.iter()
            .any(|e| e.status == nymirad::secrets::NoteStatus::Accepted)
    })
}

/// The second RPC provider for the set size, if the config names one.
fn second_rpc(cfg: &Config, transport: &Transport) -> Option<Rpc> {
    let url = cfg.rpc_url_second.clone()?;
    Some(Rpc::new(url, transport).unwrap_or_else(|e| die(e)))
}

/// The withdrawal key's SHA-256 pinned in this build. Demo builds may take it from `config.json`.
fn compiled_withdrawal_key_hash(cfg: &Config) -> String {
    #[cfg(feature = "mock")]
    if let Some(hash) = &cfg.withdrawal_key_sha256 {
        return hash.clone();
    }
    #[cfg(not(feature = "mock"))]
    let _ = cfg;
    include_str!("../pins/withdrawal-key.sha256")
        .trim()
        .to_string()
}

/// The proving key's SHA-256 pinned in this build. Demo builds may take it from `config.json`.
fn compiled_proving_key_hash(cfg: &Config) -> String {
    // The demo's setup directory holds the zkapi development keys, not the ceremony's.
    #[cfg(feature = "mock")]
    if let Some(hash) = &cfg.proving_key_sha256 {
        return hash.clone();
    }
    #[cfg(not(feature = "mock"))]
    let _ = cfg;
    include_str!("../pins/proving-key.sha256")
        .trim()
        .to_string()
}

#[cfg(feature = "mock")]
fn attestor(
    home: &std::path::Path,
    _operator_url: &str,
    _nras_direct: bool,
) -> Arc<dyn nymirad::attest::Attestor> {
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
fn attestor(
    home: &std::path::Path,
    operator_url: &str,
    nras_direct: bool,
) -> Arc<dyn nymirad::attest::Attestor> {
    use nymirad::nearai::{NearAiAttestor, NrasMode};
    match signed_policy(home) {
        Ok(policy) => {
            let mode = if nras_direct {
                NrasMode::Direct
            } else {
                NrasMode::Relay {
                    operator_url: operator_url.to_string(),
                }
            };
            Arc::new(NearAiAttestor::new(policy).with_nras_mode(mode))
        }
        Err(why) => {
            eprintln!("warning: {why}; chat requests will be refused");
            Arc::new(nymirad::attest::Unavailable)
        }
    }
}

/// The signed policy in force: the one shipped with this build, or a newer signed one in the
/// state directory. Without a pinned policy signer, a readable policy that verifies and is new
/// enough, chat is refused rather than sent to a key nobody checked.
#[cfg(not(feature = "mock"))]
fn signed_policy(home: &std::path::Path) -> Result<nymirad::attest::Policy, String> {
    let signer: Option<[u8; 32]> = hex::decode(include_str!("../pins/policy-signer.pub").trim())
        .ok()
        .and_then(|b| b.try_into().ok());
    let Some(signer) = signer else {
        return Err("this build pins no attestation policy signer".into());
    };
    // A signed policy in the state directory is used when it is newer than the one signed and
    // shipped with this build (a newer one, downloaded); an older or equal one never replaces it.
    // A file that exists but cannot be read is an error, not "no file".
    let file = match std::fs::read(home.join("attestation-policy.json")) {
        Ok(f) => Some(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("attestation-policy.json: {e}")),
    };
    match nymirad::attest::Policy::choose(
        file.as_deref(),
        include_bytes!("../pins/attestation-policy.json"),
        &signer,
        MIN_POLICY_VERSION,
    ) {
        Ok(policy) if policy.deployments.is_empty() => {
            Err("attestation-policy.json lists no deployment".into())
        }
        Ok(policy) => {
            // The shipped policy (version 4) pins NVIDIA certificates: a policy without them is
            // refused instead of checked against the JWKS key alone.
            if ENFORCE_NVIDIA_PINS {
                policy.require_nvidia_pins().map_err(|e| e.to_string())?;
            }
            Ok(policy)
        }
        Err(e) => Err(format!("attestation-policy.json: {e}")),
    }
}

/// The line `status` prints about the GPU certificate chain.
#[cfg(not(feature = "mock"))]
fn gpu_chain_line(home: &std::path::Path) -> (String, bool) {
    let policy = match signed_policy(home) {
        Ok(p) => p,
        Err(why) => return (format!("GPU chain: unknown ({why})"), true),
    };
    let chain = nymirad::nearai::gpu_chain_line(&policy);
    match nymirad::nearai::read_mode(home) {
        // No serve is running: the default applies to the next one.
        Ok(None) => (
            format!(
                "{chain}\n{} (no serve is running; this is what the next serve does)",
                nymirad::nearai::nras_mode_line(&policy, false)
            ),
            true,
        ),
        Ok(Some(direct)) => (
            format!(
                "{chain}\n{} (the running serve)",
                nymirad::nearai::nras_mode_line(&policy, direct)
            ),
            true,
        ),
        Err(why) => (
            format!("{chain}\nERROR: GPU evidence mode unknown: {why}"),
            false,
        ),
    }
}

#[cfg(feature = "mock")]
fn gpu_chain_line(_: &std::path::Path) -> (String, bool) {
    ("GPU chain: not applicable (demo build)".into(), true)
}

/// Whether a policy with no pinned NVIDIA certificate is refused. Off while the signed policy
/// that ships has none, so the live chat keeps working.
#[cfg(not(feature = "mock"))]
const ENFORCE_NVIDIA_PINS: bool = true;

/// Policies older than this are refused.
const MIN_POLICY_VERSION: u32 = 5;

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
    let Some(mut command) = (!args.is_empty()).then(|| args.remove(0)) else {
        die(USAGE)
    };
    // `agent create` is a deposit that also registers the agent; the rest only edit the registry.
    let mut agent_spec = None;
    if command == "agent" {
        let registry = Registry::at(&home);
        match (!args.is_empty()).then(|| args.remove(0)).as_deref() {
            Some("create") => {
                if args.iter().any(|a| a == "--source") {
                    die("a budget note is funded with NEAR USDC only; Intents funding is not offered for it");
                }
                agent_spec = Some(agent_spec_from(&home, &mut args));
                command = "deposit".into();
            }
            Some("disable") => {
                let name = flag(&mut args, "--name").unwrap_or_else(|| die(USAGE));
                registry.disable(&name).unwrap_or_else(|e| die(e));
                println!("Agent {name} is disabled: its next request gets 401 token_disabled.");
                return;
            }
            Some("ack-warning") => {
                let name = flag(&mut args, "--name").unwrap_or_else(|| die(USAGE));
                let book = registry.load().unwrap_or_else(|e| die(e));
                let agent = book
                    .agents
                    .iter()
                    .find(|a| a.name == name)
                    .unwrap_or_else(|| die(format!("no agent named {name}")));
                let cleared = nymirad::chargewarn::ChargeWarnings::new(&home)
                    .ack(&agent.note)
                    .unwrap_or_else(|e| die(e));
                println!(
                    "{}",
                    if cleared {
                        format!("The charge warning on agent {name} is cleared: its next request is served.")
                    } else {
                        format!("Agent {name} has no charge warning.")
                    }
                );
                return;
            }
            Some("list") => {
                let book = registry.load().unwrap_or_else(|e| die(e));
                println!("{}", agents::list_lines(&book).join("\n"));
                return;
            }
            Some("policy") => {
                policy_load(&home, mode, &registry, &mut args).await;
                return;
            }
            Some("revoke") => {
                owner_command(&home, "revoke", &mut args).await;
                return;
            }
            Some("show") => {
                agents_command(&home).await;
                return;
            }
            Some("approvals") => {
                approvals_command(&home, None, api_port(&mut args)).await;
                return;
            }
            Some("approve") => {
                let file = flag(&mut args, "--file").unwrap_or_else(|| die(USAGE));
                let bytes = std::fs::read(&file).unwrap_or_else(|e| die(format!("{file}: {e}")));
                approvals_command(&home, Some(bytes), api_port(&mut args)).await;
                return;
            }
            _ => die(USAGE),
        }
    }
    if command == "deposit" {
        eprintln!("Terms of use: run `nymirad terms`.");
    }
    if command == "deposit" && args.iter().any(|a| a == "--source") && !mode.intents_enabled() {
        die("Intents has no testnet. Quote only; fund with testnet USDC from the Circle faucet.");
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    if (command == "deposit" && args.iter().any(|a| a == "--source"))
        || (command == "recover" && args.iter().any(|a| a == "--intents"))
    {
        intents_command(&home, mode, &source, &mut args, clock).await;
        return;
    }
    match command.as_str() {
        "init" => {
            let mut cfg = Config {
                rpc_url: flag(&mut args, "--rpc").unwrap_or_else(|| die(USAGE)),
                operator_url: flag(&mut args, "--operator").unwrap_or_else(|| die(USAGE)),
                account: flag(&mut args, "--account").unwrap_or_else(|| die(USAGE)),
                rpc_url_second: flag(&mut args, "--rpc-second"),
                socks: flag(&mut args, "--socks"),
                ..Config::default()
            };
            if let Some(dir) = flag(&mut args, "--setup-dir") {
                let request = compiled_proving_key_hash(&cfg);
                let withdrawal = compiled_withdrawal_key_hash(&cfg);
                nymirad::gateway::check_proving_key(&dir, &request).unwrap_or_else(|e| die(e));
                nymirad::gateway::check_withdrawal_key(&dir, &withdrawal)
                    .unwrap_or_else(|e| die(e));
                cfg.setup_dir = Some(dir);
            }
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
        "terms" => print!("{TERMS}"),
        "print-pins" => {
            let network = flag(&mut args, "--network").unwrap_or_else(|| die(USAGE));
            let default_rpc = pins::default_rpc(&network).unwrap_or_else(|| die(USAGE));
            let vault = flag(&mut args, "--vault").unwrap_or_else(|| die(USAGE));
            let url = flag(&mut args, "--rpc").unwrap_or_else(|| default_rpc.into());
            let rpc = Rpc::new(url, &Transport::Direct).unwrap_or_else(|e| die(e));
            let live = pins::read_live(&rpc, &vault)
                .await
                .unwrap_or_else(|e| die(format!("the vault could not be read: {e}")));
            println!("{}", pins::render(&live));
            eprintln!(
                "Review these against the deployment record before committing them. The proving-key hashes are separate files: crates/nymirad/pins/proving-key.sha256 and withdrawal-key.sha256."
            );
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
            let nras_direct = switch(&mut args, "--nras-direct");
            if nras_direct {
                eprintln!("warning: --nras-direct is on: NVIDIA's attestation service sees your IP address and the time of every GPU check");
            }
            let gateway =
                build_gateway(&cfg, &home, mode, &source, clock.clone(), nras_direct).await;
            let token = Arc::new(token);
            let registry = Registry::at(&home);
            if let Some(gw) = &gateway {
                let listener = nymirad::ownersock::bind(&home).unwrap_or_else(|e| die(e));
                println!(
                    "owner socket: {}",
                    nymirad::ownersock::socket_path(&home).display()
                );
                tokio::spawn(nymirad::ownersock::serve(listener, gw.clone()));
                let book = registry.load().unwrap_or_else(|e| die(e));
                if book
                    .agents
                    .iter()
                    .any(|a| a.transport == agents::Transport::Unix)
                {
                    let dir = cfg
                        .agent_socket_dir
                        .clone()
                        .unwrap_or_else(nymirad::agentsock::default_dir);
                    let gid = book.socket_group.as_deref().map(|g| {
                        agents::group_id(g).unwrap_or_else(|| die(format!("no local group {g}")))
                    });
                    let bound = nymirad::agentsock::bind(&dir, gid).unwrap_or_else(|e| die(e));
                    println!(
                        "agent socket: {}",
                        dir.join(nymirad::agentsock::SOCKET).display()
                    );
                    let app = guard::router_for(
                        token.clone(),
                        Some(gw.clone()),
                        registry.clone(),
                        guard::Side::AgentSocket,
                    );
                    let listener = nymirad::agentsock::AgentListener::new(
                        bound,
                        registry.clone(),
                        nymirad::agentsock::socket_uid(),
                    );
                    tokio::spawn(nymirad::agentsock::serve(listener, app));
                }
            }
            // The mode file is written once the port is bound, so a second `serve` that cannot
            // bind leaves the first one's file alone; it is removed when this one stops.
            let mode_home = home.clone();
            // Demo and test builds only: tests that run the binary cannot share port 8788.
            #[cfg(feature = "mock")]
            let port = flag(&mut args, "--port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(guard::PORT);
            #[cfg(not(feature = "mock"))]
            let port = guard::PORT;
            let served = guard::serve_with(token, gateway, registry, port, move || {
                nymirad::nearai::save_mode(&mode_home, nras_direct)
            })
            .await;
            nymirad::nearai::clear_mode(&home);
            served.unwrap_or_else(|e| die(e));
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
                let gpu = gpu_chain_line(&home);
                // The "now" figure: the same rule at the current final block, from the vault
                // views only. The operator's list can only add team IDs to what is removed.
                let vault_account = pinned
                    .as_ref()
                    .map(|p| p.vault.clone())
                    .or_else(|| cfg.vault.clone());
                let now_figure = match (vault_account, notes_have_accepted(&notes)) {
                    (Some(vault), true) => Some(
                        nymirad::setsize::read_now(
                            &rpc,
                            second_rpc(&cfg, &transport).as_ref(),
                            &vault,
                            clock.now_secs(),
                        )
                        .await
                        .map(|raw| {
                            nymirad::setsize::evaluate(
                                &raw,
                                &nymirad::setsize::pinned_team(),
                                operator
                                    .as_ref()
                                    .ok()
                                    .and_then(nymirad::setsize::operator_team)
                                    .as_deref(),
                            )
                        })
                        .map_err(|e| e.to_string()),
                    ),
                    _ => None,
                };
                let book = Registry::at(&home).load().unwrap_or_else(|e| die(e));
                println!(
                    "{}",
                    status::render_with(
                        &format!("{}\n{}", transport.status_line(), gpu.0),
                        operator.as_ref(),
                        &notes.list().unwrap_or_else(|e| die(e)),
                        &book.agents,
                        &tripwire,
                        &board.view().await,
                        clock.now_secs(),
                        now_figure.as_ref()
                    )
                );
                // The last leases with the verdict on the signature and any overcharge warning:
                // the answer reached the app before the signature could be read, so this is
                // where a "not verified" shows.
                for r in nymirad::records::Records::new(&home)
                    .list()
                    .iter()
                    .rev()
                    .take(5)
                {
                    println!("{}", status::lease_line(r));
                }
                let leases = nymirad::records::Records::new(&home).list();
                let figures = now_figure.is_some()
                    || leases.iter().rev().take(5).any(|r| r.set_size.is_some());
                println!(
                    "{}",
                    status::privacy_lines(!transport.is_direct(), mode == Mode::Testnet, figures)
                );
                for w in cfg.backup_warnings() {
                    println!("warning: {w}");
                }
                if cfg.backup_target().is_some() {
                    println!("{}", backup_line(&cfg, clock.now_secs()));
                }
                // A mode nobody can read is an error, not a default.
                if !gpu.1 {
                    std::process::exit(1);
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
            let deposit_amount: Option<u128> = if command == "deposit" {
                Some(
                    flag(&mut args, "--amount")
                        .and_then(|a| a.parse().ok())
                        .unwrap_or_else(|| die(USAGE)),
                )
            } else {
                None
            };
            if mode == Mode::Testnet {
                if let Some(amount) = deposit_amount {
                    let config = rpc
                        .view(&pins.vault, "config", serde_json::json!({}))
                        .await
                        .unwrap_or_else(|e| die(e));
                    let fee: u128 = config["note_fee"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(|| die("invalid vault note fee"));
                    let total = amount
                        .checked_add(fee)
                        .unwrap_or_else(|| die("amount overflow"));
                    let quote_source = OneClick::new(BASE_URL, &transport)
                        .ok()
                        .map(|q| Arc::new(q.with_amount(total)) as Arc<dyn QuoteSource>);
                    if let nymirad::quotes::View::Lines(lines) =
                        Board::new(mode, quote_source, clock.clone()).view().await
                    {
                        for line in lines {
                            println!("{}: {} source tokens for {} NEAR USDC base units. Quote only. Intents has no testnet, so no cross-chain payment is made here. Fund with testnet USDC from the Circle faucet.", line.label, line.text, total);
                        }
                    }
                }
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
                tx: nymirad::deposit::funding_tx(
                    &cfg.rpc_url,
                    &cfg.account,
                    token,
                    key,
                    tx_store.clone(),
                    clock.clone(),
                    &transport,
                )
                .unwrap_or_else(|e| die(e)),
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
            let amount = deposit_amount.unwrap_or_else(|| die(USAGE));
            let payout = flag(&mut args, "--payout");
            let deposited = match &agent_spec {
                None => ctx.deposit(amount, payout.as_deref(), &Terminal).await,
                Some(spec) => {
                    if payout.is_some() {
                        die("a budget note pays out to your own account; --payout is not accepted");
                    }
                    let fee = ctx.pins.config["note_fee"]
                        .as_str()
                        .and_then(|f| f.parse().ok())
                        .unwrap_or(0);
                    println!(
                        "{}\n{}",
                        agents::cap_hint(fee),
                        agents::transport_note(spec.transport)
                    );
                    let (registry, owner, now) = (
                        Registry::at(&home),
                        ctx.sender.clone(),
                        ctx.clock.now_secs(),
                    );
                    let told = std::sync::Mutex::new(Vec::new());
                    let register = |commitment: &str| {
                        let made = spec.register(&registry, commitment, &owner, now)?;
                        *told.lock().unwrap_or_else(|e| e.into_inner()) = made.notes;
                        Ok(())
                    };
                    let done = ctx
                        .deposit_with(amount, Some(&owner), &Terminal, Some(&register))
                        .await;
                    for line in told.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                        println!("{line}");
                    }
                    if done.is_ok() {
                        println!("Agent {} is registered. Until you load a signed policy, every request on its token gets 402 policy_missing.", spec.name);
                    }
                    done
                }
            };
            match deposited {
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

/// The options of `agent create`.
fn agent_spec_from(home: &std::path::Path, args: &mut Vec<String>) -> agents::Spec {
    let name = flag(args, "--name").unwrap_or_else(|| die(USAGE));
    if !agents::valid_name(&name) {
        die("--name takes 1 to 32 characters of a-z, 0-9, - and _");
    }
    let transport = flag(args, "--transport")
        .and_then(|t| agents::Transport::parse(&t))
        .unwrap_or_else(|| die("--transport is unix or tcp"));
    match transport {
        agents::Transport::Tcp => agents::Spec {
            transport,
            token_file: flag(args, "--token-file")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("agents").join(format!("{name}.token"))),
            name,
            uid: None,
            group: None,
            gid: None,
        },
        agents::Transport::Unix => {
            let user = flag(args, "--agent-user").unwrap_or_else(|| {
                die("--agent-user NAME: the OS user the agent runs as (a unix token is for another user)")
            });
            let (uid, primary) = agents::user_ids(&user)
                .unwrap_or_else(|| die(format!("no local user named {user}")));
            // SAFETY: geteuid has no preconditions.
            if uid == unsafe { libc::geteuid() } {
                die("an agent that runs as your own OS user can read your owner token. Run it against a dedicated nymirad instance (its own --home, port and owner token) that holds only its budget note");
            }
            let (group, gid) = match flag(args, "--group") {
                Some(g) => {
                    let gid = agents::group_id(&g)
                        .unwrap_or_else(|| die(format!("no local group named {g}")));
                    (g, gid)
                }
                None => (
                    agents::group_name(primary).unwrap_or_else(|| die("the user has no group")),
                    primary,
                ),
            };
            let token_file = flag(args, "--token-file").map(PathBuf::from).unwrap_or_else(|| {
                die("--token-file PATH: a place the agent user can reach, outside the state directory")
            });
            agents::Spec {
                name,
                transport,
                uid: Some(uid),
                group: Some(group),
                gid: Some(gid),
                token_file,
            }
        }
    }
}

/// `agent policy`: the signed policy file, checked, shown and confirmed.
async fn policy_load(
    home: &std::path::Path,
    mode: Mode,
    registry: &Registry,
    args: &mut Vec<String>,
) {
    let name = flag(args, "--name").unwrap_or_else(|| die(USAGE));
    let file = flag(args, "--file").unwrap_or_else(|| die(USAGE));
    let bytes = std::fs::read(&file).unwrap_or_else(|e| die(format!("{file}: {e}")));
    let cfg = Config::load(home).unwrap_or_else(|e| die(e));
    let pinned = pins::compiled(mode);
    #[cfg(feature = "mock")]
    let pinned = pinned.or_else(|| pins::demo(home));
    let pinned = pinned.unwrap_or_else(|| die("no vault is pinned in this build"));
    let classes: Vec<u128> = pinned.config["bound_classes"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
        .unwrap_or_default();
    let ctx = nymirad::policy::Context::new(
        nymira_operator::proxy::ProxyConfig::default()
            .models
            .into_iter()
            .map(|m| m.id)
            .collect(),
        &classes,
    );
    let transport = connect(&cfg, home, TOR_WAIT).await;
    let mut rpcs = vec![Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e))];
    rpcs.extend(second_rpc(&cfg, &transport));
    if rpcs.len() < 2 {
        eprintln!("warning: only one RPC endpoint is configured (rpc_url_second); the key is read from that one alone");
    }
    let loaded = nymirad::policy::load(registry, &name, &bytes, &ctx, &rpcs, &|summary| {
        println!("{summary}");
        Terminal.confirm_text("Type yes to load this policy: ")
    })
    .await
    .unwrap_or_else(|e| die(e));
    println!(
        "Policy v{} is in force for agent {name} from its next request.",
        loaded.version
    );
}

/// `agent approvals` lists what waits; `agent approve` hands over a signed approval. Both talk to
/// the running daemon on loopback with the owner token.
/// The port the owner commands talk to. Demo and test builds may name another one, as `serve` can.
fn api_port(args: &mut Vec<String>) -> u16 {
    #[cfg(feature = "mock")]
    if let Some(port) = flag(args, "--port").and_then(|p| p.parse().ok()) {
        return port;
    }
    #[cfg(not(feature = "mock"))]
    let _ = args;
    guard::PORT
}

async fn approvals_command(home: &std::path::Path, signed: Option<Vec<u8>>, port: u16) {
    let token = std::fs::read_to_string(home.join("owner.token"))
        .unwrap_or_else(|e| die(format!("owner token: {e} (is `nymirad serve` set up?)")));
    let url = format!("http://127.0.0.1:{port}{}", nymirad::approvals::PATH);
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_else(|e| die(e));
    let call = match signed {
        Some(bytes) => http.post(&url).body(bytes),
        None => http.get(&url),
    };
    let reply = call
        .header("host", format!("localhost:{}", guard::PORT))
        .bearer_auth(token.trim())
        .send()
        .await
        .unwrap_or_else(|e| die(format!("cannot reach nymirad on port {port}: {e}")));
    let status = reply.status();
    let body: serde_json::Value = reply.json().await.unwrap_or_default();
    if !status.is_success() {
        die(body["error"]["message"].as_str().unwrap_or("refused"));
    }
    match body.as_array() {
        Some(list) if list.is_empty() => println!("Nothing waits for your approval."),
        Some(list) => {
            for h in list {
                println!(
                    "{}  agent {}  model {}  input {} tokens  up to {} USD  {}  expires in {} s\n  amount {} micro-USD\n  request hash {}\n  nonce {}",
                    h["approval_id"].as_str().unwrap_or(""),
                    h["agent"].as_str().unwrap_or(""),
                    h["model"].as_str().unwrap_or(""),
                    h["input_tokens"],
                    h["estimated_cost_usd"].as_str().unwrap_or(""),
                    if h["approved"] == true { "approved" } else { "waiting" },
                    h["expires_in_secs"],
                    h["amount_micro"].as_str().unwrap_or(""),
                    h["request_hash"].as_str().unwrap_or(""),
                    h["nonce"].as_str().unwrap_or(""),
                );
            }
            println!("Sign the ID, request hash, amount and nonce on your own device, then run `nymirad agent approve --file FILE`.");
        }
        None => println!("Approved {}.", body["approved"].as_str().unwrap_or("")),
    }
}

/// `agent show`: reads the running daemon with the owner token.
async fn agents_command(home: &std::path::Path) {
    let token = std::fs::read_to_string(home.join("owner.token"))
        .unwrap_or_else(|e| die(format!("owner token: {e} (is `nymirad serve` set up?)")));
    let url = format!(
        "http://127.0.0.1:{}{}",
        guard::PORT,
        nymirad::approvals::AGENTS_PATH
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_else(|e| die(e));
    let reply = http
        .get(&url)
        .bearer_auth(token.trim())
        .send()
        .await
        .unwrap_or_else(|e| die(format!("cannot reach nymirad on port {}: {e}", guard::PORT)));
    let status = reply.status();
    let body: serde_json::Value = reply.json().await.unwrap_or_default();
    if !status.is_success() {
        die(body["error"]["message"].as_str().unwrap_or("refused"));
    }
    let rows = body.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No agents.");
    }
    for r in rows {
        println!(
            "{}: token {}{}, note {} ({}), balance {} micro-USD, spent today {} micro-USD, expires at Unix time {}\n  policy {}: {}",
            r["agent"].as_str().unwrap_or(""),
            r["token"].as_str().unwrap_or(""),
            if r["revoking"] == true {
                ", revoking"
            } else if r["charge_warning"] == true {
                ", CHARGE WARNING (run `agent ack-warning`)"
            } else {
                ""
            },
            r["note_id"],
            r["note_status"].as_str().unwrap_or("?"),
            r["balance_micro"].as_str().unwrap_or("?"),
            r["spent_today_micro"].as_str().unwrap_or("?"),
            r["expires_at"],
            r["policy_version"],
            if r["policy"].is_null() { "none in force".to_string() } else { r["policy"].to_string() },
        );
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
        "revoke" => Command::Revoke {
            agent: flag(args, "--name").unwrap_or_else(|| die(USAGE)),
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
        "claim-payout" => match flag(args, "--intents") {
            Some(commitment) => Command::ClaimIntentsPayout { commitment, family },
            None => Command::ClaimPayout {
                note: note_id(args),
                family,
            },
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
    // A policy version the owner saw after the backup was taken keeps older ones out.
    while let Some(pair) = flag(args, "--policy-seen") {
        let (name, version) = pair
            .split_once('=')
            .and_then(|(n, v)| Some((n, v.parse::<u32>().ok()?)))
            .unwrap_or_else(|| die("--policy-seen takes NAME=VERSION"));
        Registry::at(home)
            .raise_policy_seen(name, version)
            .unwrap_or_else(|e| die(e));
    }
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

async fn intents_command(
    home: &std::path::Path,
    mode: Mode,
    source: &PassphraseSource,
    args: &mut Vec<String>,
    clock: Arc<dyn Clock>,
) {
    use nymirad::{
        intents,
        oneclick::{FundingClient, DIRECT_QUESTION},
    };
    if !mode.intents_enabled() {
        die("Intents has no testnet. Quote only; fund with testnet USDC from the Circle faucet.");
    }
    let cfg = Config::load(home).unwrap_or_else(|e| die(e));
    let path = cfg.intents_path(mode);
    if path == IntentsPath::Off {
        die("This build has no Intents funding path. Fund with NEAR USDC directly.");
    }
    let confirmed = Terminal.confirm_text(DIRECT_QUESTION);
    if !confirmed {
        die("1Click direct access was not confirmed; nothing sent");
    }
    let client = FundingClient::new(mode, confirmed);
    #[cfg(feature = "mock")]
    let client = match &cfg.oneclick_base {
        Some(base) => FundingClient::mock(base, mode, confirmed),
        None => client,
    };
    let client = client.unwrap_or_else(|e| die(e));
    let passphrase = source.read().unwrap_or_else(|e| die(e));
    let transport = connect(&cfg, home, TOR_WAIT).await;
    let rpc = Rpc::new(cfg.rpc_url.clone(), &transport).unwrap_or_else(|e| die(e));
    let pinned = pins::compiled(mode);
    #[cfg(feature = "mock")]
    let pinned = pinned.or_else(|| pins::demo(home));
    let funding = intents::Funding {
        mode,
        home: home.to_path_buf(),
        rpc,
        archival: cfg
            .archival_rpc_url
            .as_ref()
            .map(|url| Rpc::new(url, &transport).unwrap_or_else(|e| die(e))),
        pins: pinned.unwrap_or_else(|| die("no vault is pinned in this build; deposits are off")),
        notes: NoteStore::open(&home.join("notes.sealed"), &passphrase, ROUNDS)
            .unwrap_or_else(|e| die(e)),
        backup: cfg
            .backup_target()
            .filter(|b| !b.places.is_empty())
            .unwrap_or_else(|| die("set up an encrypted backup before Intents funding")),
        clock,
    };
    let resumed = flag(args, "--intents");
    let is_resume = resumed.is_some();
    // The two-step path signs from the user's own account, so it needs the funding key.
    let sender = |args: &mut Vec<String>| {
        let key = funding_key(&flag(args, "--key-file").unwrap_or_else(|| die(USAGE)));
        let store = Store::open(&home.join("tx.db")).unwrap_or_else(|e| die(e));
        let token = funding.pins.config["token"].as_str().unwrap_or_default();
        nymirad::deposit::funding_tx(
            &cfg.rpc_url,
            &cfg.account,
            token.into(),
            key,
            store,
            funding.clock.clone(),
            &transport,
        )
        .unwrap_or_else(|e| die(e))
    };
    let mut tx = None;
    let mut attempt = if let Some(commitment) = resumed {
        funding
            .notes
            .funding_attempts()
            .unwrap_or_else(|e| die(e))
            .into_iter()
            .find(|a| a.commitment == commitment)
            .inspect(|a| {
                if a.via.is_some() != (path == IntentsPath::TwoStep) {
                    die("that attempt used the other Intents path; this build offers only one");
                }
                if a.via.is_some() {
                    tx = Some(sender(args));
                }
            })
            .unwrap_or_else(|| die("no saved Intents attempt for that commitment"))
    } else {
        let chosen = flag(args, "--source").unwrap_or_else(|| die(USAGE));
        let amount = flag(args, "--amount")
            .and_then(|v| v.parse::<u128>().ok())
            .unwrap_or_else(|| die(USAGE));
        let config = funding.check(amount).await.unwrap_or_else(|e| die(e));
        if path == IntentsPath::TwoStep {
            let sender = sender(args);
            funding
                .two_step_ready(&sender, &config)
                .await
                .unwrap_or_else(|e| die(e));
            tx = Some(sender);
        }
        let offered = funding
            .sources(&client, amount)
            .await
            .unwrap_or_else(|e| die(e));
        println!(
            "Available sources: {}",
            offered
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if chosen == "list" {
            return;
        }
        let asset = offered
            .iter()
            .find(|s| s.name == chosen)
            .map(|s| s.asset)
            .unwrap_or_else(|| {
                die("the source has no valid dry quote or its minimum exceeds this payment")
            });
        let payout = flag(args, "--payout")
            .unwrap_or_else(|| die("--payout NEAR_ACCOUNT is required for Intents funding"));
        let refund =
            flag(args, "--refund").unwrap_or_else(|| die("--refund SOURCE_WALLET is required"));
        let registered = funding
            .payout(&payout, &config)
            .await
            .unwrap_or_else(|e| die(e));
        println!("Payout account {} (fingerprint {}). USDC registered: {}. Consider a fresh payout account.\nDenomination {} base units + non-refundable note fee {} base units.\nRefund to your own source wallet: {}.\nA close through the operator costs {} USDC base units. An exit or payout claim needs a little NEAR for gas on the submitter account.", payout, nymirad::deposit::fingerprint(&payout), registered, amount, config["note_fee"], refund.escape_default(), config["close_fee"]);
        if tx.is_some() {
            println!(
                "{}\nYour account {} pays the vault in the second step.",
                intents::TWO_STEP_NOTICE,
                cfg.account
            );
        }
        if !Terminal.confirm_text("Type yes to confirm this payout account and refund address: ") {
            die("payout account not confirmed; no quote requested");
        }
        let backup_key = flag(args, "--backup-key-file")
            .map(|p| {
                PassphraseSource::File(p.into())
                    .read()
                    .unwrap_or_else(|e| die(e))
            })
            .unwrap_or_else(|| secret_input(None, "Backup private key or passphrase: "));
        match &tx {
            Some(tx) => {
                funding
                    .prepare_two_step(&client, tx, amount, asset, &refund, &payout, &backup_key)
                    .await
            }
            None => {
                funding
                    .prepare(&client, amount, asset, &refund, &payout, &backup_key)
                    .await
            }
        }
        .unwrap_or_else(|e| die(e))
    };
    if !is_resume {
        let q = &attempt.quote["quote"];
        println!("{}", intents::PRIVACY);
        if attempt.via.is_some() {
            println!("{}", intents::PRIVACY_TWO_STEP);
        }
        println!("Pay {} source tokens ({} source base units); delivered {} NEAR USDC ({} base units) (denomination plus note fee). This is the all-in cost. The documented keyless fee is 25 basis points of the amount delivered and is part of that cost; the quote does not itemise it.\nDeadline: {}.\nOther listed fees: refundFee={}, withdrawFee={}, appFees={}.\nThe signature shows only that 1Click issued this quote. 1Click and its solvers hold the funds in transit; the refund address is the only way back.", q["amountInFormatted"].as_str().unwrap_or("unknown").escape_default(), q["amountIn"].as_str().unwrap_or("unknown").escape_default(), q["amountOutFormatted"].as_str().unwrap_or("unknown").escape_default(), q["amountOut"].as_str().unwrap_or("unknown").escape_default(), q["deadline"].as_str().unwrap_or("unknown").escape_default(), q["refundFee"], q["withdrawFee"], attempt.quote["quoteRequest"]["appFees"]);
        println!("Deposit address: {}\nDeposit memo: {}\nPending note saved and encrypted backup restored successfully.\nTo resume after restart: recover --intents {}", q["depositAddress"].as_str().unwrap_or("missing").escape_default(), q["depositMemo"].as_str().unwrap_or("none").escape_default(), attempt.commitment);
        if !Terminal.confirm_text("Pay from your own source wallet. Type yes to check progress: ") {
            die("payment not confirmed; the pending note and quote remain stored");
        }
        #[cfg(feature = "mock")]
        if switch(args, "--mock-pay") {
            client
                .mock_pay(q["depositAddress"].as_str().unwrap_or(""))
                .await
                .unwrap_or_else(|e| die(e));
        }
    }
    for _ in 0..120 {
        let step = match &tx {
            Some(tx) => funding.progress_two_step(&client, tx, &mut attempt).await,
            None => funding.progress(&client, &mut attempt).await,
        };
        match step {
            Ok(Some(accepted)) => {
                println!("{}", accepted.report());
                for warning in accepted.warnings {
                    eprintln!("warning: {warning}");
                }
                println!("Deposit receipt: https://nearblocks.io/txns/{}#{}\nRead the payout account in that receipt.", accepted.tx_hash, attempt.receipt.as_deref().unwrap_or(""));
                return;
            }
            Ok(None) => {
                println!(
                    "{}",
                    attempt.notice.as_deref().unwrap_or(
                        "Waiting for the destination transaction and final vault receipt."
                    )
                )
            }
            Err(e) => die(e),
        }
    }
    die("still pending; resume with recover --intents COMMITMENT");
}
