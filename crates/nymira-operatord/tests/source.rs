//! Source rules of the binary: no secret is read from the environment, and the roles keep to
//! their own keys.

use std::path::Path;

fn source(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name)).unwrap()
}

#[test]
fn nothing_in_the_binary_reads_the_environment() {
    for file in ["main.rs", "roles.rs", "config.rs", "hardening.rs", "lib.rs"] {
        let text = source(file);
        assert!(!text.contains("env::var"), "{file} reads the environment");
        assert!(!text.contains("env::args_os") || file == "main.rs");
    }
}

/// The text of one role's function, up to the next role.
fn role(name: &str) -> String {
    let text = source("roles.rs");
    let start = text
        .find(&format!("pub async fn {name}("))
        .unwrap_or_else(|| panic!("no role {name}"));
    let rest = &text[start + 10..];
    let end = rest.find("\npub async fn ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn each_role_loads_only_its_own_keys() {
    let loads = |text: &str| -> Vec<String> {
        text.match_indices("near_key(cfg, \"")
            .chain(text.match_indices("seed(cfg, \""))
            .map(|(at, m)| {
                let tail = &text[at + m.len()..];
                tail[..tail.find('"').unwrap()].to_string()
            })
            .collect()
    };
    assert!(loads(&role("api")).is_empty(), "the API loads no key");
    assert_eq!(loads(&role("minter")), ["usage"]);
    assert_eq!(loads(&role("signer")), ["heartbeat"]);
    assert_eq!(loads(&role("closer")), ["mutual_close"]);
    let mut watcher = loads(&role("watcher"));
    watcher.sort();
    assert_eq!(watcher, ["answer", "entry", "upkeep"]);
    // The NEAR AI login is built in the minter and nowhere else.
    for other in ["api", "signer", "closer", "watcher"] {
        assert!(
            !role(other).contains("NearAiAdapter"),
            "{other} holds the login"
        );
    }
    // Passphrases are asked for in the signer and the minter only.
    for other in ["api", "closer", "watcher"] {
        assert!(
            !role(other).contains("passphrase("),
            "{other} asks for a passphrase"
        );
    }
}
