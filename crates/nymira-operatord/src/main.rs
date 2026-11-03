//! `nymira-operatord <role> --config FILE`: one of the five operator processes, or a small
//! tool (`emergency`, `keygen`).

use std::path::PathBuf;

use nymira_operator::secretfile;
use nymira_operator::store::{self, Store};
use nymira_operatord::{config::Config, hardening, roles};

const USAGE: &str = "usage: nymira-operatord <command> --config FILE
processes:  api | minter | signer | closer | watcher | replica | deadman (second machine)
tools:      emergency set <notice> | emergency show | emergency clear
            recovery hold | recovery clear | recovery show
                               put this machine on recovery hold (after a restore from a backup
                               or when the second machine was replaced), lift it, or read it
            keygen <name>      writes a new 32-byte secret file and prints its public key
            pubkeys            asks for the state and clearance passphrases and prints the
                               operator_pk and clearance_pk the vault's new() takes";

fn die(message: impl std::fmt::Display) -> ! {
    eprintln!("nymira-operatord: {message}");
    std::process::exit(1)
}

fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.remove(at);
    (at < args.len()).then(|| args.remove(at))
}

#[tokio::main]
async fn main() {
    // A panic message can hold request data, and stderr goes to a log file: print the place only.
    std::panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        eprintln!("nymira-operatord: panic at {at}");
    }));
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let Some(config_path) = flag(&mut args, "--config") else {
        die(USAGE)
    };
    let cfg = Config::load(&PathBuf::from(config_path)).unwrap_or_else(|e| die(e));
    if let Err(e) = hardening::apply() {
        die(format!("hardening: {e}"));
    }
    let command = args.first().cloned().unwrap_or_else(|| die(USAGE));
    let result = match command.as_str() {
        "api" => roles::api(&cfg).await,
        "minter" => roles::minter(&cfg).await,
        "signer" => roles::signer(&cfg).await,
        "closer" => roles::closer(&cfg).await,
        "watcher" => roles::watcher(&cfg).await,
        "replica" => roles::replica(&cfg).await,
        "deadman" => roles::deadman(&cfg).await,
        "emergency" => emergency(&cfg, &args[1..]).await,
        "recovery" => recovery(&cfg, &args[1..]).await,
        "keygen" => keygen(&cfg, args.get(1).map(String::as_str)),
        "pubkeys" => pubkeys(&cfg),
        _ => Err(USAGE.to_string()),
    };
    if let Err(e) = result {
        die(e);
    }
}

async fn emergency(cfg: &Config, args: &[String]) -> Result<(), String> {
    let store = Store::open(&cfg.db()).map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("set") => {
            let notice = args.get(1).ok_or("emergency set <notice>")?.clone();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs();
            store
                .run(move |c| store::set_emergency(c, &notice, now))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("emergency set: new leases and clearances stop now");
        }
        Some("show") => {
            let notice = store
                .run(|c| store::emergency(c))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("{}", notice.as_deref().unwrap_or("no emergency"));
        }
        Some("clear") => {
            store
                .run(|c| store::clear_emergency(c))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("emergency cleared");
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

async fn recovery(cfg: &Config, args: &[String]) -> Result<(), String> {
    let store = Store::open(&cfg.db()).map_err(|e| e.to_string())?;
    match args.first().map(String::as_str) {
        Some("hold") => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs();
            store
                .run(move |c| store::set_recovery_hold(c, now))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("recovery hold set: nothing is signed or pushed until `recovery clear`");
        }
        Some("clear") => {
            store
                .run(|c| store::clear_recovery_hold(c))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("recovery hold cleared");
        }
        Some("show") => {
            let since = store
                .run(|c| store::recovery_hold(c))
                .await
                .map_err(|e| e.to_string())?;
            match since {
                Some(t) => println!("on hold since {t}"),
                None => println!("not on hold"),
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

/// Prints the two public keys the vault's `new()` takes, from the same two passphrases the
/// signer asks for at start-up.
fn pubkeys(cfg: &Config) -> Result<(), String> {
    use nymira_operator::signer;
    let state = secretfile::passphrase("state key passphrase").map_err(|e| e.to_string())?;
    let clearance =
        secretfile::passphrase("clearance key passphrase").map_err(|e| e.to_string())?;
    let (operator_pk, clearance_pk) =
        signer::public_keys_from_passphrases(&state, &clearance, signer::KDF_ROUNDS, cfg.sandbox())
            .map_err(|e| e.to_string())?;
    let point = |p: &zkapi_types::wire::CurvePointWire| {
        serde_json::json!({
            "x": format!("0x{}", hex::encode(p.x.0)),
            "y": format!("0x{}", hex::encode(p.y.0)),
        })
    };
    println!(
        "{}",
        serde_json::json!({ "operator_pk": point(&operator_pk), "clearance_pk": point(&clearance_pk) })
    );
    Ok(())
}

fn keygen(cfg: &Config, name: Option<&str>) -> Result<(), String> {
    use ed25519_dalek::SigningKey;
    use rand::RngCore;
    let name = name.ok_or("keygen <name>")?;
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    secretfile::write(&cfg.secret(name), hex::encode(seed).as_bytes())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
    );
    Ok(())
}
