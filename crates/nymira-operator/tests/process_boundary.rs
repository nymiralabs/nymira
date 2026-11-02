//! The process boundary: who can reach a socket or read a secret, and one owner for the log.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::ipc;
use nymira_operator::replica::{FakeReplica, Replica, ReplicaEntry};
use nymira_operator::replica_socket::{serve, SocketReplica};
use nymira_operator::secretfile;

fn uid() -> u32 {
    // SAFETY: no arguments, no failure mode.
    unsafe { libc::getuid() }
}

fn entry(n: u32) -> ReplicaEntry {
    ReplicaEntry {
        nullifier: format!("n{n}"),
        lease_id: format!("l{n}"),
        request_json: "{}".into(),
        bound: 100_000,
        client_request_id: format!("c{n}"),
        reserved_at: 3_600,
    }
}

#[tokio::test]
async fn two_processes_write_one_log_through_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replica.sock");
    let owner = Arc::new(FakeReplica::default());
    tokio::spawn(serve(ipc::bind(&path).unwrap(), owner.clone(), uid()));
    // The public API and the signer each hold a client, not a log.
    let (api, signer) = (SocketReplica::new(&path), SocketReplica::new(&path));
    api.append(&entry(1)).await.unwrap();
    signer
        .record("signed", "l1", Some("{\"x\":1}"))
        .await
        .unwrap();
    api.append(&entry(2)).await.unwrap();
    assert_eq!(
        owner
            .entries()
            .iter()
            .map(|e| e.lease_id.as_str())
            .collect::<Vec<_>>(),
        ["l1", "l2"]
    );
    assert_eq!(owner.records(), [("signed".into(), "l1".into())]);
    assert_eq!(
        api.recorded("signed", "l1").await.as_deref(),
        Some("{\"x\":1}")
    );

    // The owner's recovery flag and health reach the clients.
    assert!(SocketReplica::refresh(&api).await.is_ok());
    assert!(!api.in_recovery());
    owner.set_recovery(true);
    api.sync().await.unwrap();
    assert!(api.in_recovery());
    owner.set_down(true);
    assert!(signer.append(&entry(3)).await.is_err());
}

#[tokio::test]
async fn a_client_is_not_ready_until_the_owner_has_answered() {
    let dir = tempfile::tempdir().unwrap();
    let client = SocketReplica::new(&dir.path().join("nobody.sock"));
    assert!(
        client.in_recovery(),
        "nothing may be signed before the first answer"
    );
    assert_eq!(client.health(), "unreachable");
    assert!(client.append(&entry(1)).await.is_err());
}

#[tokio::test]
async fn the_owner_drops_a_peer_of_another_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replica.sock");
    let owner = Arc::new(FakeReplica::default());
    tokio::spawn(serve(ipc::bind(&path).unwrap(), owner.clone(), uid() + 1));
    let client = SocketReplica::new(&path);
    assert!(client.append(&entry(1)).await.is_err());
    assert!(owner.entries().is_empty());
}

#[tokio::test]
async fn a_socket_is_0600_and_garbage_gets_a_refusal_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replica.sock");
    let owner = Arc::new(FakeReplica::default());
    tokio::spawn(serve(ipc::bind(&path).unwrap(), owner.clone(), uid()));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let reply: serde_json::Value = ipc::call(
        &path,
        &serde_json::json!({"op": "drop_all_tables"}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(reply["ok"], false);
    assert!(owner.entries().is_empty());
}

#[test]
fn a_secret_file_is_0400_and_a_loose_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys").join("nearai.token");
    secretfile::write(&path, b"token").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(&**secretfile::read(&path).unwrap(), b"token");
    assert!(secretfile::write(&path, b"again").is_err(), "no overwrite");

    for mode in [0o440, 0o404, 0o600, 0o644, 0o666] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(secretfile::read(&path).is_err(), "mode {mode:o} was read");
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(
        secretfile::read(&link).is_err(),
        "a symlink is not a secret file"
    );
}

#[test]
fn a_passphrase_comes_from_a_line_and_not_from_the_environment() {
    let got =
        secretfile::passphrase_from(std::io::Cursor::new(b"correct horse\n".to_vec())).unwrap();
    assert_eq!(&*got, "correct horse");
    assert!(secretfile::passphrase_from(std::io::Cursor::new(b"\n".to_vec())).is_err());
    assert!(secretfile::passphrase_from(std::io::Cursor::new(Vec::new())).is_err());
    // No source file of the operator reads a passphrase from the environment.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            assert!(
                !code.contains("env::var")
                    || path.ends_with("nearai.rs")
                    || path.ends_with("sealed.rs"),
                "{} reads the environment",
                path.display()
            );
        }
    }
}

// ---------------------------------------------------------------- the state key stays in the signer

use common::{Rig, Setup};
use nymira_operator::signer::{bind_socket, serve as serve_signer, SocketStateSigner};
use nymira_operator::status::{status_message, StateSigning};
use zkapi_proof::compact::verify_signature;

#[tokio::test(flavor = "multi_thread")]
async fn the_status_nonce_is_signed_by_the_signer_and_by_nothing_else_it_is_asked_for() {
    let rig = Rig::new(Setup::default()).await;
    let path = rig.dir.path().join("signer.sock");
    tokio::spawn(serve_signer(
        bind_socket(&path).unwrap(),
        rig.signer.clone(),
        uid(),
    ));
    let remote = SocketStateSigner::connect(&path).await.unwrap();
    assert_eq!(remote.public_key(), rig.signer.state_public_key());

    let now = rig.clock_now() / 5 * 5;
    let nonce = "ab".repeat(16);
    let sig = remote
        .sign_status(&nonce, "operator.test", now)
        .await
        .unwrap();
    assert!(verify_signature(
        &rig.signer.state_public_key(),
        &status_message(&nonce, "operator.test", now),
        &sig
    )
    .unwrap());
    // Another host, a stale slot, an off-grid time and a nonce of the wrong shape are refused.
    assert!(remote.sign_status(&nonce, "evil.test", now).await.is_none());
    assert!(remote
        .sign_status(&nonce, "operator.test", now - 300)
        .await
        .is_none());
    assert!(remote
        .sign_status(&nonce, "operator.test", now + 1)
        .await
        .is_none());
    assert!(remote
        .sign_status("zz", "operator.test", now)
        .await
        .is_none());
    assert!(remote
        .sign_status(&"AB".repeat(16), "operator.test", now)
        .await
        .is_none());
}
