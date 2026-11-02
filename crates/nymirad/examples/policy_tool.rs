//! The offline release step for `attestation-policy.json`.
//!
//!   policy_tool keygen KEY_FILE
//!       makes the policy signing key (32 random bytes, hex, mode 0600) and prints the public key
//!       to put in `crates/nymirad/pins/policy-signer.pub`. Keep KEY_FILE off the network.
//!   policy_tool sign KEY_FILE DRAFT OUT VERSION
//!       signs a draft (from `record_evidence`, checked by a person against NEAR AI's published
//!       builds) as policy VERSION and writes the file clients download.

use std::os::unix::fs::OpenOptionsExt;

use ed25519_dalek::SigningKey;
use nymirad::attest::Policy;

fn die(m: impl std::fmt::Display) -> ! {
    eprintln!("policy_tool: {m}");
    std::process::exit(1)
}

fn load_key(path: &str) -> SigningKey {
    let seed: [u8; 32] = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| hex::decode(t.trim()).ok())
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| die("the key file must hold 32 bytes in hex"));
    SigningKey::from_bytes(&seed)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["keygen", key_file] => {
            let key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(key_file)
                .unwrap_or_else(|e| die(format!("{key_file}: {e}")));
            std::io::Write::write_all(&mut file, hex::encode(key.to_bytes()).as_bytes())
                .unwrap_or_else(|e| die(e));
            println!("{}", hex::encode(key.verifying_key().to_bytes()));
        }
        ["sign", key_file, draft, out, version] => {
            let version: u32 = version
                .parse()
                .unwrap_or_else(|_| die("VERSION is a number"));
            let mut policy: Policy = serde_json::from_slice(
                &std::fs::read(draft).unwrap_or_else(|e| die(format!("{draft}: {e}"))),
            )
            .unwrap_or_else(|e| die(format!("{draft}: {e}")));
            policy.version = version;
            let signer = load_key(key_file);
            let signed = policy.sign(&signer);
            // Load it back the way a client does, so a bad draft fails here and not on a user's machine.
            let json = serde_json::to_vec_pretty(&signed).expect("policy serializes");
            Policy::load(&json, &signer.verifying_key().to_bytes(), version)
                .unwrap_or_else(|e| die(format!("the signed policy does not load: {e}")));
            std::fs::write(out, json).unwrap_or_else(|e| die(format!("{out}: {e}")));
            println!("wrote {out}");
        }
        _ => die("usage: policy_tool keygen KEY_FILE | sign KEY_FILE DRAFT OUT VERSION"),
    }
}
