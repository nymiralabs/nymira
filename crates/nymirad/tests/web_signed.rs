//! Files the web app's Agents screen signed in a browser, read the way `nymirad` reads them.
//! The Playwright test `web/e2e/agents.spec.ts` runs this with the file it got from the page:
//!   NYMIRA_WEB_SIGNED_KIND   policy | approval
//!   NYMIRA_WEB_SIGNED_FILE   the signed file
//!   NYMIRA_WEB_AGENT, NYMIRA_WEB_NOTE, NYMIRA_WEB_ACCOUNT   the agent, its note commitment, the owner
//!   NYMIRA_WEB_RPC           the (fake) NEAR RPC that holds the owner's access keys
//!   NYMIRA_WEB_MESSAGE       approval only: the exact message the page was asked to sign
//! Without them the test does nothing, so a plain `cargo test` stays offline.

use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::{approvals, policy};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[tokio::test]
async fn a_file_the_web_app_signed_is_accepted() {
    let (Some(kind), Some(file)) = (env("NYMIRA_WEB_SIGNED_KIND"), env("NYMIRA_WEB_SIGNED_FILE"))
    else {
        eprintln!("NYMIRA_WEB_SIGNED_FILE is not set: nothing to check");
        return;
    };
    let (agent, note, account, rpc_url) = (
        env("NYMIRA_WEB_AGENT").expect("agent"),
        env("NYMIRA_WEB_NOTE").expect("note"),
        env("NYMIRA_WEB_ACCOUNT").expect("account"),
        env("NYMIRA_WEB_RPC").expect("rpc"),
    );
    let bytes = std::fs::read(&file).unwrap();
    let rpcs = vec![Rpc::new(rpc_url, &Net::Direct).unwrap()];
    match kind.as_str() {
        "policy" => {
            let home = tempfile::tempdir().unwrap();
            let registry = Registry::at(home.path());
            Spec {
                name: agent.clone(),
                transport: Transport::Tcp,
                uid: None,
                group: None,
                gid: None,
                token_file: home.path().join("agent.token"),
            }
            .register(&registry, &note, &account, 1)
            .unwrap();
            let ctx = policy::Context::new(
                nymira_operator::proxy::ProxyConfig::default()
                    .models
                    .into_iter()
                    .map(|m| m.id)
                    .collect(),
                &[100_000, 500_000],
            );
            // The same call `nymirad agent policy` makes: signature, content, key on chain, confirm.
            let loaded = policy::load(&registry, &agent, &bytes, &ctx, &rpcs, &|_| true)
                .await
                .unwrap_or_else(|e| panic!("nymirad refused the policy: {e}"));
            assert!(loaded.policy.require_tee);
            assert_eq!(loaded.version, 1);
        }
        "approval" => {
            // `Approvals::approve` needs a held request with a random nonce, so the signature, the
            // recipient, the message fields and the key are checked here with the same functions.
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let s = |k: &str| v[k].as_str().unwrap_or_else(|| panic!("no {k}")).to_string();
            assert_eq!(v.as_object().unwrap().len(), 6);
            assert_eq!(s("account_id"), account);
            assert_eq!(s("recipient"), approvals::recipient(&agent, &note));
            let b64 = |t: &str| base64::engine::general_purpose::STANDARD.decode(t).unwrap();
            let key: [u8; 32] = bs58::decode(s("public_key").strip_prefix("ed25519:").unwrap())
                .into_vec()
                .unwrap()
                .try_into()
                .unwrap();
            let nonce: [u8; 32] = b64(&s("nonce")).try_into().unwrap();
            let sig: [u8; 64] = b64(&s("signature")).try_into().unwrap();
            VerifyingKey::from_bytes(&key)
                .unwrap()
                .verify_strict(
                    &policy::nep413_hash(&s("message"), &nonce, &s("recipient")),
                    &Signature::from_bytes(&sig),
                )
                .expect("the signature does not match the message");
            let message: serde_json::Value = serde_json::from_str(&s("message")).unwrap();
            let expected: serde_json::Value =
                serde_json::from_str(&env("NYMIRA_WEB_MESSAGE").expect("message")).unwrap();
            assert_eq!(message, expected);
            assert_eq!(message["kind"], approvals::KIND);
            policy::check_key(&rpcs, &account, &s("public_key"))
                .await
                .expect("not a full-access key of the owner");
        }
        other => panic!("unknown kind {other}"),
    }
}
