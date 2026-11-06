//! The set size against scripted providers: the count, the team lists, two providers, the stored
//! figure against the "now" figure, and the lines `nymirad status` prints from them.

use nymira_fake_near_rpc::{ArgsMatch, FakeRpc, ViewResult};
use nymirad::records::{LeaseRecord, Records};
use nymirad::rpc::{Rpc, Transport};
use nymirad::setsize::{self, SetSize};
use nymirad::status;
use serde_json::{json, Value};

/// The vault the testnet build pins.
const VAULT: &str = "vault.nymira.testnet";
const FAR: u64 = 4_000_000_000;
/// The request time of the lease in these tests, in Unix seconds.
const T: u64 = 1_800_000_000;

fn rec(id: u32, status: &str, expiry: u64) -> Value {
    json!({
        "note_id": id, "deposit_amount": "1000000", "expiry": expiry,
        "status": status, "leaf_retired": status == "Closed",
    })
}

fn active(ids: std::ops::Range<u32>) -> Vec<Value> {
    ids.map(|i| rec(i, "Active", FAR)).collect()
}

/// A provider that serves `records` for a read of the first `leaf_count` notes, from the next
/// block on.
fn script(fake: &FakeRpc, leaf_count: u32, records: Vec<Value>) {
    fake.advance_blocks(1, 1.0);
    fake.set_view(
        VAULT,
        "leaf_count",
        ArgsMatch::Any,
        ViewResult::json(json!(leaf_count)),
    );
    let mut from = 0;
    while from < leaf_count {
        let limit = 100.min(leaf_count - from);
        let page: Vec<Value> = records
            .iter()
            .skip(from as usize)
            .take(limit as usize)
            .cloned()
            .collect();
        fake.set_view(
            VAULT,
            "note_records",
            ArgsMatch::json(json!({ "from": from, "limit": limit })),
            ViewResult::json(json!(page)),
        );
        from += limit;
    }
}

fn rpc(fake: &FakeRpc) -> Rpc {
    Rpc::new(fake.url(), &Transport::Direct).unwrap()
}

/// The lease-time read: the leaf count and the block come from the provider's head now.
async fn at_lease(
    a: &FakeRpc,
    b: Option<&FakeRpc>,
    leaf_count: u32,
    t: u64,
) -> Result<setsize::Raw, setsize::SetSizeError> {
    let (hash, _) = rpc(a).final_block().await.unwrap();
    let second = b.map(rpc);
    setsize::read(&rpc(a), second.as_ref(), VAULT, &hash, leaf_count, t).await
}

fn size(raw: &setsize::Raw, pinned: &[u32], operator: Option<&[u32]>) -> SetSize {
    setsize::evaluate(raw, pinned, operator)
}

async fn pair(leaf_count: u32, records: Vec<Value>) -> (FakeRpc, FakeRpc) {
    let (a, b) = (FakeRpc::start().await, FakeRpc::start().await);
    script(&a, leaf_count, records.clone());
    script(&b, leaf_count, records);
    (a, b)
}

#[tokio::test]
async fn six_notes_three_of_them_the_teams_give_three_external() {
    let (a, b) = pair(6, active(0..6)).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    // The team's IDs come from the pin; the operator lists the same three.
    let s = size(&raw, &[0, 1, 2], Some(&[0, 1, 2]));
    assert_eq!((s.external, s.total), (3, 6));
    assert_eq!(s.headline(), "set size: 3 (external), total 6");
    assert!(s.differences().is_empty());
    assert!(s.providers_differ.is_none() && s.one_provider.is_none());
}

#[tokio::test]
async fn expired_closed_and_exiting_notes_drop_out() {
    let mut notes = active(0..6);
    notes[1] = rec(1, "Active", T - 1); // expired just before the request
    notes[2] = rec(2, "PendingExit", FAR);
    notes[3] = rec(3, "Closed", FAR);
    notes[4] = rec(4, "Active", T); // expiry equal to the request time still counts
    let (a, b) = pair(6, notes).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    let s = size(&raw, &[], Some(&[]));
    assert_eq!((s.external, s.total), (3, 3), "notes 0, 4 and 5");
}

#[tokio::test]
async fn a_note_deposited_after_the_leases_root_does_not_count() {
    // Seven notes exist, the proof's root holds six. The vault returns six for a read of six.
    let (a, b) = pair(6, active(0..6)).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    assert_eq!(size(&raw, &[], Some(&[])).total, 6);
    // Once the root takes note 6 in, a read at the new leaf count sees it.
    script(&a, 7, active(0..7));
    script(&b, 7, active(0..7));
    let now = setsize::read_now(&rpc(&a), Some(&rpc(&b)), VAULT, T)
        .await
        .unwrap();
    assert_eq!(size(&now, &[], Some(&[])).total, 7);
}

#[tokio::test]
async fn a_close_after_the_lease_leaves_the_stored_figure_and_lowers_now() {
    let (a, b) = pair(6, active(0..6)).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    let stored = size(&raw, &[0, 1, 2], Some(&[0, 1, 2]));
    assert_eq!(stored.external, 3);
    // Note 4 closes on chain after the lease.
    let mut after = active(0..6);
    after[4] = rec(4, "Closed", FAR);
    script(&a, 6, after.clone());
    script(&b, 6, after);
    let now = setsize::read_now(&rpc(&a), Some(&rpc(&b)), VAULT, T)
        .await
        .unwrap();
    let now = size(&now, &[0, 1, 2], Some(&[0, 1, 2]));
    assert_eq!((now.external, now.total), (2, 5));

    // `nymirad status` shows both, and the stored one is what the record kept.
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(dir.path());
    let mut lease = lease_record("l1");
    lease.set_size = Some(stored);
    records.write(&lease).unwrap();
    let read_back = &records.list()[0];
    let line = status::lease_line(read_back);
    assert!(line.contains("set size: 3 (external), total 6"), "{line}");
    let text = status_text(Ok(now));
    assert!(
        text.contains("now: set size: 2 (external), total 5"),
        "{text}"
    );
}

#[tokio::test]
async fn an_operator_that_leaves_a_pinned_id_out_does_not_raise_n() {
    let (a, b) = pair(6, active(0..6)).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    let s = size(&raw, &[0, 1, 2], Some(&[0, 1]));
    assert_eq!((s.external, s.total), (3, 6));
    let lease = {
        let mut l = lease_record("l");
        l.set_size = Some(s);
        l
    };
    let line = status::lease_line(&lease);
    assert!(
        line.contains("team IDs only in this release's pinned list: 2"),
        "{line}"
    );
}

#[tokio::test]
async fn an_operator_that_adds_an_id_lowers_n() {
    let (a, b) = pair(6, active(0..6)).await;
    let raw = at_lease(&a, Some(&b), 6, T).await.unwrap();
    let s = size(&raw, &[0, 1, 2], Some(&[0, 1, 2, 3]));
    assert_eq!((s.external, s.total), (2, 6));
    let mut lease = lease_record("l");
    lease.set_size = Some(s);
    assert!(status::lease_line(&lease).contains("team IDs only in the operator's list: 3"));
}

#[tokio::test]
async fn two_providers_that_disagree_give_the_lower_figure_and_show_it() {
    let (a, b) = pair(6, active(0..6)).await;
    let mut fewer = active(0..6);
    fewer[5] = rec(5, "Closed", FAR);
    script(&b, 6, fewer);
    // Both providers must serve the block the read names, so `a` moves to the same height.
    script(&a, 6, active(0..6));
    let (hash, _) = rpc(&a).final_block().await.unwrap();
    let raw = setsize::read(&rpc(&a), Some(&rpc(&b)), VAULT, &hash, 6, T)
        .await
        .unwrap();
    assert_eq!(raw.sets.len(), 2);
    let s = size(&raw, &[], Some(&[]));
    assert_eq!((s.external, s.total), (5, 5));
    assert_eq!(s.providers_differ, Some([(6, 6), (5, 5)]));
    assert!(s
        .differences()
        .iter()
        .any(|d| d.contains("disagree (set size 6 and 5)")));
}

#[tokio::test]
async fn a_second_provider_that_fails_or_is_missing_is_said() {
    let (a, b) = pair(3, active(0..3)).await;
    let alone = at_lease(&a, None, 3, T).await.unwrap();
    assert_eq!(alone.sets.len(), 1);
    let s = size(&alone, &[], Some(&[]));
    assert_eq!(s.total, 3);
    assert!(s.differences()[0].contains("read from one RPC provider only"));
    b.set_failure(nymira_fake_near_rpc::Failure::Http(503));
    let down = at_lease(&a, Some(&b), 3, T).await.unwrap();
    assert_eq!(down.sets.len(), 1);
    assert!(down.one_provider.is_some());
    // The first provider failing is an error: no figure is made from nothing.
    let (hash, _) = rpc(&a).final_block().await.unwrap();
    a.set_failure(nymira_fake_near_rpc::Failure::Http(503));
    assert!(setsize::read(&rpc(&a), None, VAULT, &hash, 3, T)
        .await
        .is_err());
}

#[tokio::test]
async fn more_than_one_page_is_read_from_zero() {
    let (a, b) = pair(150, active(0..150)).await;
    let raw = at_lease(&a, Some(&b), 150, T).await.unwrap();
    assert_eq!(size(&raw, &[], Some(&[])).total, 150);
    let asked: Vec<Value> = a
        .requests()
        .iter()
        .filter(|r| r.params["method_name"] == "note_records")
        .map(|r| r.params["args_base64"].clone())
        .collect();
    assert_eq!(asked.len(), 2, "two pages of at most 100");
}

#[tokio::test]
async fn a_provider_that_skips_or_repeats_a_note_is_an_error() {
    let a = FakeRpc::start().await;
    let mut skipped = active(0..3);
    skipped.remove(1);
    skipped.push(rec(5, "Active", FAR));
    script(&a, 3, skipped);
    assert!(at_lease(&a, None, 3, T).await.is_err());
}

fn lease_record(id: &str) -> LeaseRecord {
    serde_json::from_value(json!({
        "lease_id": id, "note_id": 5, "root": "0x01", "leaf_count": 6, "snapshot_block": 10,
        "bound_class": "100000", "opened_at": T, "closed_at": null, "charge": null,
        "expected_charge": null, "model": "m", "sampling": {}, "requests": 0, "message_count": 0,
        "message_bytes": 0, "prompt_tokens": 0, "completion_tokens": 0, "trust": null,
        "warnings": [],
    }))
    .unwrap()
}

fn status_text(now: Result<SetSize, String>) -> String {
    use nymirad::secrets::{NoteEntry, NoteStatus};
    let note = NoteEntry {
        chain_label: "near-testnet".into(),
        secret: "00".into(),
        commitment: "0xc0".into(),
        denomination: "1000000".into(),
        payout: "alice.test".into(),
        status: NoteStatus::Accepted,
        note_id: Some(5),
        expiry_ns: Some(T + 30 * 86_400),
        deposit_amount: Some("1050000".into()),
        tx_hash: None,
        created_at: T,
    };
    status::render(
        "Tor: ready",
        Ok(&json!({"vault": VAULT, "open_notes": 12, "external_notes": 7})),
        &[note],
        &nymirad::pins::Tripwire::default(),
        &nymirad::quotes::View::Lines(vec![]),
        T,
        Some(&now),
    )
}

#[test]
fn the_raw_open_note_count_never_appears_on_its_own() {
    let text = status_text(Ok(SetSize {
        external: 2,
        total: 5,
        ..SetSize::default()
    }));
    // The operator's own `open_notes` and `external_notes` are not printed.
    assert!(
        !text.contains("12") && !text.contains("open notes"),
        "{text}"
    );
    assert!(!text.contains("external_notes"), "{text}");
    // Every figure comes as "set size: N (external)" with the total after it.
    assert!(text.contains("set size: 2 (external), total 5"), "{text}");
}

#[test]
fn a_now_figure_that_cannot_be_read_says_so() {
    let text = status_text(Err("rpc transport: refused".into()));
    assert!(
        text.contains("now: set size: unavailable (rpc transport: refused)"),
        "{text}"
    );
}

#[test]
fn the_status_sentences_come_from_the_shared_file() {
    let shared: Value =
        serde_json::from_str(include_str!("../../../shared/privacy-strings.json")).unwrap();
    let say = |key: &str| shared[key].as_str().unwrap().to_string();
    for (tor, line) in [(true, "tor_on"), (false, "tor_off")] {
        let text = status::privacy_lines(tor, false, false);
        assert!(text.starts_with(&say(line)), "{text}");
        assert!(text.contains(&say("tor_limits")));
        // Without a figure there is nothing to qualify.
        assert!(!text.contains(&say("five_not_safe")));
    }
    assert_eq!(say("tor_on"), "`nymirad` traffic goes through Tor");
    assert_eq!(say("tor_off"), "Tor off");

    let mainnet = status::privacy_lines(true, false, true);
    for key in ["balances_private", "includes_unlisted", "five_not_safe"] {
        assert!(mainnet.contains(&say(key)), "{key}: {mainnet}");
    }
    assert!(!mainnet.contains(&say("testnet_notes")));
    let testnet = status::privacy_lines(true, true, true);
    assert!(testnet.contains(&say("testnet_notes")), "{testnet}");
    assert!(say("includes_unlisted")
        .starts_with("N includes notes the operator or the team control and did not list"));
    assert!(say("five_not_safe").contains("5 is still not a safe number"));
    let testnet_text = say("testnet_notes");
    for part in [
        "test notes",
        "free testnet USDC",
        "anyone can add them at no cost",
        "the set is small",
    ] {
        assert!(testnet_text.contains(part), "{part}");
    }
}

#[test]
fn the_backup_carries_the_stored_figure() {
    use nymirad::backup::StateBackup;
    let dir = tempfile::tempdir().unwrap();
    let mut lease = lease_record("l1");
    lease.set_size = Some(SetSize {
        external: 3,
        total: 6,
        pinned_only: vec![2],
        ..SetSize::default()
    });
    Records::new(dir.path()).write(&lease).unwrap();
    let mut backup = StateBackup {
        version: nymirad::backup::VERSION,
        ..StateBackup::default()
    };
    backup.add_file(dir.path(), nymirad::records::FILE);
    let back = StateBackup::from_bytes(&backup.to_bytes().unwrap()).unwrap();

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&back.files[nymirad::records::FILE])
        .unwrap();
    let restored = tempfile::tempdir().unwrap();
    std::fs::write(restored.path().join(nymirad::records::FILE), bytes).unwrap();
    let got = &Records::new(restored.path()).list()[0];
    assert_eq!(got.set_size, lease.set_size);
}

/// The binary as a user runs it: `nymirad status` over scripted providers.
#[tokio::test(flavor = "multi_thread")]
async fn the_status_command_prints_the_stored_and_the_now_figures() {
    use nymirad::secrets::{NoteEntry, NoteStatus, NoteStore, ROUNDS};
    use std::os::unix::fs::PermissionsExt;

    let (a, b) = pair(3, active(0..3)).await;
    let home = tempfile::tempdir().unwrap();
    nymirad::config::Config {
        rpc_url: a.url(),
        rpc_url_second: Some(b.url()),
        operator_url: "http://127.0.0.1:1".into(),
        account: "alice.test".into(),
        direct: true,
        ..Default::default()
    }
    .save(home.path())
    .unwrap();
    let pass = home.path().join("pass");
    std::fs::write(&pass, "test-passphrase").unwrap();
    std::fs::set_permissions(&pass, std::fs::Permissions::from_mode(0o600)).unwrap();
    NoteStore::open(&home.path().join("notes.sealed"), "test-passphrase", ROUNDS)
        .unwrap()
        .add(NoteEntry {
            chain_label: "near-testnet".into(),
            secret: "00".into(),
            commitment: "0xc0".into(),
            denomination: "1000000".into(),
            payout: "alice.test".into(),
            status: NoteStatus::Accepted,
            note_id: Some(2),
            expiry_ns: Some(FAR),
            deposit_amount: Some("1050000".into()),
            tx_hash: None,
            created_at: T,
        })
        .unwrap();
    let mut lease = lease_record("l1");
    lease.set_size = Some(SetSize {
        external: 4,
        total: 6,
        ..SetSize::default()
    });
    Records::new(home.path()).write(&lease).unwrap();

    let home_path = home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_nymirad"))
            .arg("--home")
            .arg(&home_path)
            .arg("--passphrase-file")
            .arg(home_path.join("pass"))
            .arg("status")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // The lease keeps the figure it was opened with; the note shows the current one.
    assert!(text.contains("set size: 4 (external), total 6"), "{text}");
    assert!(
        text.contains("now: set size: 3 (external), total 3"),
        "{text}"
    );
    // The test build runs direct, so the Tor line says so; the rest follows from the strings.
    assert!(text.contains("\nTor off\n"), "{text}");
    let shared: Value =
        serde_json::from_str(include_str!("../../../shared/privacy-strings.json")).unwrap();
    for key in [
        "tor_limits",
        "balances_private",
        "includes_unlisted",
        "five_not_safe",
        "testnet_notes",
    ] {
        assert!(
            text.contains(shared[key].as_str().unwrap()),
            "{key}: {text}"
        );
    }
    // Both providers were asked for the notes.
    for fake in [&a, &b] {
        assert!(fake
            .requests()
            .iter()
            .any(|r| r.params["method_name"] == "note_records"));
    }
}
