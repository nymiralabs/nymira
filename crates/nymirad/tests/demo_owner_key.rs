//! `make demo` signs the demo policy with a test owner key that exists only in mock builds.

#[path = "../examples/demo_sign.rs"]
#[allow(dead_code)]
mod demo_sign;
#[path = "support/policy.rs"]
mod policy;

use ed25519_dalek::SigningKey;
use nymirad::agents::{Registry, Spec, Transport};

#[test]
fn the_demo_policy_loads_as_an_owner_would_sign_it() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    Spec {
        name: "ironclaw".into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: home.path().join("ironclaw.token"),
    }
    .register(&registry, "0xabc123", "demo.test", 1)
    .unwrap();
    let book = registry.load().unwrap();
    let agent = &book.agents[0];
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let signed = demo_sign::sign(&key, "demo.test", "ironclaw", "0xabc123", 1);
    let loaded =
        nymirad::policy::check_signed(&serde_json::to_vec(&signed).unwrap(), agent, &policy::ctx())
            .unwrap();
    assert_eq!(loaded.policy.allowed_models, ["z-ai/glm-5.3-flash"]);
    assert!(!loaded.policy.require_tee);
}

fn manifest() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap()
}

#[test]
fn the_signing_tool_is_built_only_with_the_mock_feature() {
    let m = manifest();
    let stanza = m
        .split("[[example]]")
        .find(|s| s.contains("\"demo_sign\""))
        .unwrap();
    assert!(stanza
        .lines()
        .take(3)
        .any(|l| l.trim() == "required-features = [\"mock\"]"));
    // `mock` is not a default feature, so a plain or release build leaves the tool out.
    assert!(!m.contains("default = ["));
}

#[test]
fn the_daemon_itself_has_no_demo_key_or_signing_code() {
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for word in ["demo_sign", "funding.key", "DEMO_POLICY"] {
                assert!(!text.contains(word), "{} mentions {word}", path.display());
            }
        }
    }
}
