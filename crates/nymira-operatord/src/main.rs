//! `nymira-operatord <role> --config FILE`: one of the five operator processes, or a small
//! tool (`emergency`, `keygen`).

use std::path::PathBuf;

use nymira_operator::secretfile;
use nymira_operator::store::{self, Store};
use nymira_operatord::{config::Config, hardening, roles};

const USAGE: &str = "usage: nymira-operatord <command> --config FILE
processes:  api | minter | signer | closer | watcher | replica (second machine)
tools:      emergency set <notice> | emergency show | emergency clear
            keygen <name>      writes a new 32-byte secret file and prints its public key";

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
        "emergency" => emergency(&cfg, &args[1..]).await,
        "keygen" => keygen(&cfg, args.get(1).map(String::as_str)),
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
