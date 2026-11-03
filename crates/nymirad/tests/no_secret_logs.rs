//! No secret reaches stderr: not the note secret, the sealing passphrase or a lease token. The
//! gateway runs a chat (its answer carries a lease token that a recording proxy captures), an
//! exit and the background loop's failed finalize, with the process's stderr (fd 2) redirected
//! to a file. The libtest harness holds back what test threads print (and what the threads they
//! spawn print), so the test runs itself again with `--nocapture` and the scenario inside.

#[path = "support/stack.rs"]
mod stack;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use near_workspaces::types::NearToken;
use nymira_operator::clock::Clock;
use nymirad::gateway::ChatOptions;
use nymirad::rpc::Transport;
use serde_json::json;
use stack::*;
use tokio::io::AsyncWriteExt;

/// A TCP forwarder that keeps every byte the target sends back.
async fn recording_proxy(target: String) -> (String, Arc<Mutex<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let log = kept.clone();
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let (target, log) = (target.clone(), log.clone());
            tokio::spawn(async move {
                let Ok(up) = tokio::net::TcpStream::connect(target).await else {
                    return;
                };
                let (mut cr, mut cw) = client.into_split();
                let (mut ur, mut uw) = up.into_split();
                let to_server = tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut cr, &mut uw).await;
                });
                let mut buf = [0u8; 8192];
                while let Ok(n) = tokio::io::AsyncReadExt::read(&mut ur, &mut buf).await {
                    if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    log.lock().unwrap().extend_from_slice(&buf[..n]);
                }
                to_server.abort();
            });
        }
    });
    (format!("http://{addr}"), kept)
}

fn lease_token(captured: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(captured);
    let at = text.find("\"lease_token\":\"")? + "\"lease_token\":\"".len();
    Some(text[at..].split('"').next()?.to_string())
}

/// Redirects fd 2 to a file; returns the file's path and the saved descriptor.
fn capture_stderr(path: &std::path::Path) -> libc::c_int {
    use std::os::unix::io::IntoRawFd;
    let file = std::fs::File::create(path).unwrap().into_raw_fd();
    // SAFETY: plain descriptor calls on descriptors this test owns.
    unsafe {
        let saved = libc::dup(2);
        assert!(saved >= 0 && libc::dup2(file, 2) >= 0);
        libc::close(file);
        saved
    }
}

const CHILD: &str = "NYMIRA_LOG_TEST_CHILD";
const NAME: &str = "secrets_and_the_lease_token_never_reach_stderr";

#[test]
fn secrets_and_the_lease_token_never_reach_stderr() {
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(scenario());
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let said =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the scenario failed:\n{said}");
    assert!(
        said.contains("test result: ok. 1 passed"),
        "the scenario did not run:\n{said}"
    );
}

async fn scenario() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let note_secret = entry(&s).secret.clone();
    let (proxy_url, captured) = recording_proxy(
        s.parts
            .operator_url
            .trim_start_matches("http://")
            .to_string(),
    )
    .await;
    let proxied = GatewayParts {
        operator_url: proxy_url,
        gateway_url: s.parts.gateway_url.clone(),
        rpc_url: s.parts.rpc_url.clone(),
        vault_id: s.parts.vault_id.clone(),
    };
    let log_path = s.home.path().join("stderr.log");
    let saved = capture_stderr(&log_path);

    // A lease opens through the proxy: the token is in the answer the gateway got.
    let g = make_gateway(s.home.path(), &proxied, Transport::Direct, s.clock.clone()).await;
    let answer = g
        .chat(
            json!({"model": MODEL, "messages": [{"role": "user", "content": "hi"}]}),
            ChatOptions::default(),
        )
        .await
        .map_err(|e| e.message)
        .unwrap();
    drop(answer);
    let token = lease_token(&captured.lock().unwrap()).expect("the operator sent a lease token");
    assert!(token.len() >= 16, "{token}");

    // The process dies mid-lease; the restart exits the note, and its background loop then tries
    // finalize_exit (the sandbox cannot wait out the window, so the vault refuses and it warns).
    let restarted = make_gateway(s.home.path(), &s.parts, Transport::Direct, s.clock.clone()).await;
    let exit = restarted
        .exit_note(0, None, None)
        .await
        .expect("exit filed");
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));
    restarted.spawn_background(Duration::from_millis(50));
    // A line from a worker thread shows the capture reaches output the harness does not hold.
    tokio::spawn(async { eprintln!("capture-control-line") });

    let mut log = String::new();
    for _ in 0..200 {
        log = std::fs::read_to_string(&log_path).unwrap_or_default();
        if log.contains("finalize_exit for note 0 waits") && log.contains("capture-control-line") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // SAFETY: restores the descriptor saved above.
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }

    assert!(
        log.contains("capture-control-line"),
        "stderr was not captured: {log:?}"
    );
    assert!(
        log.contains("finalize_exit for note 0 waits"),
        "the background loop never logged: {log:?}"
    );
    let needles = [
        ("note secret", note_secret.as_str()),
        ("sealing passphrase", PASS),
        ("lease token", token.as_str()),
    ];
    for (what, needle) in needles {
        assert!(!log.contains(needle), "the {what} reached stderr:\n{log}");
        // The check can fail: the same text with the needle appended is caught.
        assert!(format!("{log}{needle}").contains(needle));
    }
}
