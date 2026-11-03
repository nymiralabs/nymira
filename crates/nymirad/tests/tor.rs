//! The Tor process manager against a stand-in executable: hash check, private copy, flags,
//! bootstrap states, a process that dies.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nymirad::rpc::{client, RpcError, Transport};
use nymirad::tor::{Backend, Tor, TorConfig, TorError, TorState};
use sha2::{Digest, Sha256};

/// Prints the bootstrap lines C tor prints, records how it was started, then waits.
const FAKE_TOR: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "Tor version 0.4.8.12."; exit 0; fi
echo "$0" > "$MARKER_DIR/ran"
echo "$@" > "$MARKER_DIR/args"
echo $$ > "$MARKER_DIR/pid"
echo "Nov 02 12:00:00.000 [notice] Bootstrapped 10% (conn): Connecting"
echo "Nov 02 12:00:05.000 [notice] Bootstrapped 100% (done): Done"
while :; do sleep 1; done
"#;

const DYING_TOR: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "Tor version 0.4.8.12."; exit 0; fi
echo "Nov 02 12:00:00.000 [warn] Could not bind to 127.0.0.1:9050: Address already in use"
exit 1
"#;

fn package(dir: &Path, script: &str) -> (PathBuf, String) {
    let path = dir.join("tor-package");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (path, hex::encode(Sha256::digest(script.as_bytes())))
}

fn config(dir: &Path, script: &str) -> TorConfig {
    let (package, sha256) = package(dir, script);
    TorConfig {
        backend: Backend::CTor,
        package,
        sha256,
        work_dir: dir.join("work"),
    }
}

// The script reads MARKER_DIR from the environment, which `start` clears. Each test therefore
// bakes the directory into the script.
fn script_for(marker: &Path, body: &str) -> String {
    body.replace("$MARKER_DIR", &marker.display().to_string())
}

#[tokio::test]
async fn starts_a_hashed_copy_and_reaches_ready() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    std::fs::create_dir(&marker).unwrap();
    let cfg = config(dir.path(), &script_for(&marker, FAKE_TOR));
    let tor = Tor::start(&cfg).await.unwrap();
    assert_eq!(tor.version(), "Tor version 0.4.8.12");
    tor.wait_ready(Duration::from_secs(10)).await.unwrap();
    assert_eq!(tor.state(), TorState::Ready);
    assert!(tor.socks_addr().ip().is_loopback());
    assert!(tor.status_line().contains("0.4.8.12") && tor.status_line().ends_with("ready"));

    // The bytes that were hashed ran, from the private copy, with isolation on.
    let ran = std::fs::read_to_string(marker.join("ran")).unwrap();
    assert!(ran
        .trim()
        .starts_with(cfg.work_dir.join("bin").to_str().unwrap()));
    let args = std::fs::read_to_string(marker.join("args")).unwrap();
    assert!(args.contains(&format!(
        "127.0.0.1:{} IsolateSOCKSAuth",
        tor.socks_addr().port()
    )));
    assert!(args.contains("--ClientOnly 1"));
    assert!(args.contains(&format!("--__OwningControllerProcess {}", std::process::id())));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(cfg.work_dir.join("bin/tor"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o500);

    // Dropping the handle stops the process.
    let pid = std::fs::read_to_string(marker.join("pid"))
        .unwrap()
        .trim()
        .to_string();
    drop(tor);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "tor is still running after the handle was dropped");
}

#[tokio::test]
async fn a_package_that_does_not_match_the_pin_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    std::fs::create_dir(&marker).unwrap();
    let mut cfg = config(dir.path(), &script_for(&marker, FAKE_TOR));
    cfg.sha256 = "00".repeat(32);
    let Err(TorError::PackageHash { pinned, found, .. }) = Tor::start(&cfg).await else {
        panic!("a wrong hash must be refused");
    };
    assert_eq!(pinned, "00".repeat(32));
    assert_ne!(found, pinned);
    assert!(!marker.join("ran").exists() && !cfg.work_dir.join("bin/tor").exists());
}

#[tokio::test]
async fn a_build_without_a_pin_does_not_start_tor() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), "#!/bin/sh\n");
    cfg.sha256 = "  ".into();
    assert!(matches!(
        Tor::start(&cfg).await,
        Err(TorError::NotPinned("tor"))
    ));
    cfg.package = dir.path().join("missing");
    cfg.sha256 = "ab".repeat(32);
    assert!(matches!(
        Tor::start(&cfg).await,
        Err(TorError::Package { .. })
    ));
}

#[tokio::test]
async fn a_dead_tor_is_down_and_nothing_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), DYING_TOR);
    let tor = Tor::start(&cfg).await.unwrap();
    let Err(TorError::NotReady(why)) = tor.wait_ready(Duration::from_secs(10)).await else {
        panic!("a process that exited is not ready");
    };
    assert!(
        why.contains("exited") && why.contains("Address already in use"),
        "{why}"
    );
    assert!(tor.status_line().contains("DOWN"));
    let transport = Transport::Tor(std::sync::Arc::new(tor));
    let Err(RpcError::TorDown(_)) = client(&transport) else {
        panic!("no client may be built while Tor is down");
    };
}

#[tokio::test]
async fn a_slow_bootstrap_times_out_as_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("m");
    std::fs::create_dir(&marker).unwrap();
    let slow = script_for(&marker, FAKE_TOR).replace("Bootstrapped 100%", "Bootstrapped 60%");
    let tor = Tor::start(&config(dir.path(), &slow)).await.unwrap();
    assert!(matches!(
        tor.wait_ready(Duration::from_millis(800)).await,
        Err(TorError::NotReady(_))
    ));
    assert_eq!(tor.state(), TorState::Starting(60));
    let Err(RpcError::TorDown(why)) = client(&Transport::Tor(std::sync::Arc::new(tor))) else {
        panic!("starting is not ready");
    };
    assert!(why.contains("60%"));
}

/// Runs the real `tor` named by `NYMIRA_REAL_TOR` (its SHA-256 in `NYMIRA_REAL_TOR_SHA256`) through
/// the manager and sends one request over it. Needs the network: `cargo test -p nymirad --test tor
/// -- --ignored real_tor`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real tor package and the network"]
async fn real_tor_bootstraps_and_carries_a_request() {
    let (Ok(package), Ok(sha256)) = (
        std::env::var("NYMIRA_REAL_TOR"),
        std::env::var("NYMIRA_REAL_TOR_SHA256"),
    ) else {
        panic!("set NYMIRA_REAL_TOR and NYMIRA_REAL_TOR_SHA256");
    };
    let dir = tempfile::tempdir().unwrap();
    let tor = Tor::start(&TorConfig {
        backend: Backend::CTor,
        package: package.into(),
        sha256,
        work_dir: dir.path().join("work"),
    })
    .await
    .expect("the real tor starts");
    tor.wait_ready(Duration::from_secs(180))
        .await
        .expect("the real tor bootstraps");
    eprintln!("ready: {:?}, {}", tor.state(), tor.version());
    let started = std::time::Instant::now();
    // The transport owns the process: it must outlive the requests.
    let transport = Transport::Tor(std::sync::Arc::new(tor));
    let http = client(&transport).unwrap();
    let mut last = None;
    for _ in 0..8 {
        match http.get("https://check.torproject.org/api/ip").send().await {
            Ok(r) => {
                eprintln!("ok after {:?}", started.elapsed());
                last = Some(r.text().await.unwrap());
                break;
            }
            Err(e) => {
                eprintln!("try failed after {:?}: {e:?}", started.elapsed());
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    let body = last.expect("a request over tor");
    assert!(body.contains("\"IsTor\":true"), "{body}");
    drop(transport);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pin_file_with_several_hashes_accepts_any_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), FAKE_TOR);
    cfg.sha256 = format!("{}\n{}\n", "ab".repeat(32), cfg.sha256);
    let tor = Tor::start(&cfg).await.expect("the second hash matches");
    drop(tor);
    cfg.sha256 = format!("{}\n{}\n", "ab".repeat(32), "cd".repeat(32));
    assert!(matches!(
        Tor::start(&cfg).await,
        Err(TorError::PackageHash { .. })
    ));
}
