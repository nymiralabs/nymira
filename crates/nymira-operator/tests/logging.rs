//! The logging policy: no IP address, no prompt and no credential in any row the operator
//! writes, and no way for the source to print one.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::{json, Value};

const IP: &str = "203.0.113.77";
const SECRET: &str = "sk-test-SECRETKEY1234567890";
const PROMPT_HEX: &str = "c0ffeec0ffeec0ffeec0ffee";

/// Every cell of every table, as text (blobs as hex and as lossy text).
async fn dump(rig: &Rig) -> String {
    rig.op
        .store()
        .run(|c| {
            let tables: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut out = String::new();
            for table in tables {
                let mut stmt = c.prepare(&format!("SELECT * FROM \"{table}\""))?;
                let columns = stmt.column_count();
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    for i in 0..columns {
                        use rusqlite::types::ValueRef;
                        match row.get_ref(i)? {
                            ValueRef::Text(t) => out.push_str(&String::from_utf8_lossy(t)),
                            ValueRef::Blob(b) => {
                                out.push_str(&hex::encode(b));
                                out.push_str(&String::from_utf8_lossy(b));
                            }
                            ValueRef::Integer(n) => out.push_str(&n.to_string()),
                            _ => {}
                        }
                        out.push('\n');
                    }
                }
            }
            Ok(out)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_with_a_known_ip_and_prompt_leaves_neither_in_any_row() {
    let rig = Rig::new(Setup::default()).await;
    let (s, lease, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201, "{lease}");
    let (id, token) = (
        lease["lease_id"].as_str().unwrap(),
        lease["lease_token"].as_str().unwrap(),
    );
    let content = format!("{}{}", "ab".repeat(72), PROMPT_HEX);
    let chat = json!({
        "model": "z-ai/glm-5.3-flash",
        "messages": [{"role": "user", "content": content}],
        "max_tokens": 10,
        "stream": false,
    });
    let r = rig
        .http
        .post(format!("{}/v1/chat/completions", rig.url))
        .header("x-signing-algo", "ed25519")
        .header("x-client-pub-key", "11".repeat(32))
        .header("x-model-pub-key", "22".repeat(32))
        .header("x-encryption-version", "2")
        .header("x-nymira-lease-id", id)
        .header("x-nymira-lease-token", token)
        .header("x-forwarded-for", IP)
        .header("tailscale-ingress-src", format!("{IP}:51234"))
        .header("authorization", format!("Bearer {SECRET}"))
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{:?}", r.text().await);
    // Retire and settle, so every table the lifecycle writes has been written.
    rig.clock.advance_secs(700);
    rig.op.tick().await;
    rig.clock.advance_secs(700);
    rig.op.tick().await;

    let mut everything = dump(&rig).await;
    for (kind, key) in rig.replica.records() {
        everything.push_str(&format!("{kind}{key}\n"));
    }
    for entry in rig.replica.entries() {
        everything.push_str(&format!("{entry:?}"));
    }
    for alarm in rig.sink.sent() {
        everything.push_str(&format!("{}{}{}", alarm.key, alarm.subject, alarm.body));
    }
    everything.push_str(&rig.status().await.to_string());
    let low = everything.to_lowercase();
    assert!(everything.len() > 1000, "the dump is empty");
    for needle in [IP, PROMPT_HEX, SECRET, "sk-", "rpt-", "bearer"] {
        assert!(!low.contains(&needle.to_lowercase()), "{needle} is stored");
    }
    // The token itself is stored only as a hash.
    assert!(!everything.contains(token));
}

#[tokio::test(flavor = "multi_thread")]
async fn alarm_text_is_scrubbed_before_it_leaves() {
    let rig = Rig::new(Setup::default()).await;
    nymira_operator::alarm::Alarms::new(vec![rig.sink.clone()], rig.clock.clone())
        .raise(
            "k",
            "provider said sk-live-abcdef123456",
            "Authorization: Bearer rpt-0123456789abcdef failed",
        )
        .await;
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1);
    let text = format!("{}{}", sent[0].subject, sent[0].body);
    assert!(!text.contains("sk-live") && !text.contains("rpt-") && !text.contains("Bearer"));
}

/// Source files of a crate, tests and examples excluded.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The part of a source file that is not a `#[cfg(test)]` module.
fn production(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

/// Whether `word` occurs in `code` not as the tail of a longer identifier (`replog::` is not `log::`).
fn names(code: &str, word: &str) -> bool {
    code.match_indices(word).any(|(at, _)| {
        code[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

#[test]
fn the_operator_source_prints_nothing_and_never_reads_an_address() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 20);
    let forbidden = [
        "println!",
        "eprintln!",
        "print!(",
        "eprint!(",
        "dbg!(",
        "tracing::",
        "log::",
        "env_logger",
        "peer_addr",
        "remote_addr",
        "ConnectInfo",
        "x-forwarded-for",
        "tailscale-ingress-src",
        "x-real-ip",
    ];
    let mut hits = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        // `redact.rs` names the secrets it removes; its own tests print nothing.
        let code = production(&text);
        for word in forbidden {
            if names(&code.to_lowercase(), &word.to_lowercase()) {
                hits.push(format!("{}: {word}", file.display()));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}
