//! The `nymirad` binary as a user runs it: what it prints and refuses before it touches a chain.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};

use nymirad::backup::SECRET_ONLY_NOTICE;

fn private_file(dir: &std::path::Path, name: &str, text: &str, mode: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

/// Runs the binary with `stdin` and no controlling terminal (the confirmation then reads stdin
/// in this build, and never blocks on the developer's own terminal).
fn run(home: &std::path::Path, stdin: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nymirad"));
    cmd.arg("--home").arg(home).args(args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn initialized() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let out = run(
        home.path(),
        "",
        &[
            "init",
            "--rpc",
            "http://127.0.0.1:1",
            "--operator",
            "http://127.0.0.1:1",
            "--account",
            "alice.test",
            "--no-backup",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    home
}

#[test]
fn restoring_from_a_secret_alone_prints_the_notice_and_writes_nothing_without_a_yes() {
    let home = initialized();
    let secret = private_file(home.path(), "secret", "00aabb\n", 0o600);
    let pass = private_file(home.path(), "pass", "correct horse battery\n", 0o600);
    let args = [
        "--passphrase-file",
        pass.to_str().unwrap(),
        "restore-secret",
        "--note",
        "0",
        "--secret-file",
        secret.to_str().unwrap(),
    ];

    for answer in ["no\n", "\n", ""] {
        let out = run(home.path(), answer, &args);
        assert!(!out.status.success(), "{answer:?} must not continue");
        let said = text(&out.stdout);
        assert!(said.contains(SECRET_ONLY_NOTICE), "{said}");
        assert!(
            said.contains("Payout account: alice.test."),
            "the payout default is shown: {said}"
        );
        assert!(text(&out.stderr).contains("not confirmed; nothing written"));
        assert!(!home.path().join("notes.sealed").exists());
    }
}

#[test]
fn a_secret_file_others_can_read_is_refused_before_the_notice() {
    let home = initialized();
    let secret = private_file(home.path(), "secret", "00aabb\n", 0o644);
    let pass = private_file(home.path(), "pass", "correct horse battery\n", 0o600);
    let out = run(
        home.path(),
        "yes\n",
        &[
            "--passphrase-file",
            pass.to_str().unwrap(),
            "restore-secret",
            "--note",
            "0",
            "--secret-file",
            secret.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("readable by others (mode 644)"),
        "{}",
        text(&out.stderr)
    );
    assert!(!text(&out.stdout).contains(SECRET_ONLY_NOTICE));
}

#[test]
fn a_passphrase_file_others_can_read_stops_note_secret() {
    let home = initialized();
    let pass = private_file(home.path(), "pass", "correct horse battery\n", 0o640);
    let out = run(
        home.path(),
        "yes\n",
        &[
            "--passphrase-file",
            pass.to_str().unwrap(),
            "note-secret",
            "--note",
            "0",
        ],
    );
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("readable by others (mode 640)"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn testnet_refuses_intents_before_loading_keys_or_contacting_services() {
    let home = initialized();
    let out = run(
        home.path(),
        "yes\n",
        &[
            "deposit",
            "--source",
            "ethereum-usdc",
            "--amount",
            "1000000",
        ],
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("Terms of use: run `nymirad terms`."));
    assert!(
        text(&out.stderr).contains("Intents has no testnet"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn terms_prints_the_terms_file_and_needs_no_home() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), "", &["terms"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let terms = include_str!("../../../TERMS.md");
    assert_eq!(text(&out.stdout), terms);
    assert!(!home.path().join("config.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn print_pins_writes_the_format_of_the_compiled_pin_file() {
    use nymira_fake_near_rpc::{AccessKeySpec, AccountSpec, ArgsMatch, FakeRpc, ViewResult};
    use serde_json::json;

    let rpc = FakeRpc::start().await;
    let vault = "vault.nymira.testnet";
    rpc.set_account(
        vault,
        AccountSpec {
            code_hash: "9xCodeHash".into(),
            ..AccountSpec::default()
        },
    );
    rpc.set_access_keys(
        vault,
        vec![AccessKeySpec {
            public_key: "ed25519:Key".into(),
            nonce: 0,
            permission: json!("FullAccess"),
        }],
    );
    rpc.set_view(
        vault,
        "config",
        ArgsMatch::Any,
        ViewResult::json(json!({ "owner": "owner.testnet", "slot_rate": "5", "tvl_cap": "1" })),
    );
    rpc.set_view(
        vault,
        "vk_hashes",
        ArgsMatch::Any,
        ViewResult::json(json!({ "request": "aa", "withdrawal": "bb" })),
    );

    let home = tempfile::tempdir().unwrap();
    let url = rpc.url();
    let out = run(
        home.path(),
        "",
        &[
            "print-pins",
            "--network",
            "testnet",
            "--rpc",
            &url,
            "--vault",
            vault,
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let pins: nymirad::pins::Pins = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(pins.vault, vault);
    assert_eq!(pins.code_hash, "9xCodeHash");
    assert_eq!(pins.vk_hashes["request"], "aa");
    assert_eq!(pins.config["owner"], "owner.testnet");
    assert!(
        pins.config.get("tvl_cap").is_none(),
        "moving values are not pinned"
    );
    assert_eq!(pins.access_keys[0]["public_key"], "ed25519:Key");
    assert!(text(&out.stderr).contains("proving-key.sha256"));
}

#[test]
fn print_pins_needs_a_vault_and_a_known_network() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["print-pins", "--network", "testnet"],
        vec!["print-pins", "--network", "devnet", "--vault", "v.testnet"],
    ] {
        let out = run(home.path(), "", &args);
        assert!(!out.status.success());
        assert!(text(&out.stderr).contains("usage"), "{}", text(&out.stderr));
    }
}
