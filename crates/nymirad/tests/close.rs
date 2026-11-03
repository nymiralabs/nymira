//! Close, exit, finalize and claim-payout against a sandbox vault, the real operator (with its
//! closer stubbed) and `nymirad`'s gateway. Chain transactions come from a submitter account
//! whose access key lists the four vault methods and nothing else.

#[path = "support/stack.rs"]
mod stack;

use nymira_operator::clock::Clock;
use nymira_operator::store::Store;
use nymirad::submitter::METHODS;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use near_workspaces::types::NearToken;
use nymirad::gateway::close::{direct_text, Ask, CloseOptions, LeaveError};
use nymirad::gateway::ChatOptions;
use nymirad::rpc::Transport;
use nymirad::secrets::NoteStatus;
use serde_json::{json, Value};
use stack::*;

/// Answers every direct-send question the same way and keeps what was asked.
struct Scripted {
    yes: bool,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(yes: bool) -> Self {
        Self {
            yes,
            asked: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Ask for Scripted {
    async fn confirm_direct(&self, text: &str) -> bool {
        self.asked.lock().unwrap().push(text.into());
        self.yes
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn the_direct_send_question_has_fixed_words() {
    assert_eq!(
        direct_text(7, "250000"),
        "Tor is down. Sending this transaction directly shows your IP address to the RPC endpoint, together with this note's ID (7) and its total spend (250000 base units of the token). Type yes to send directly, or anything else to stop."
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_goes_out_from_the_submitter_and_pays_the_payout_account() {
    let s = stack(Transport::Direct).await;
    let sub = submitter(&s, "personal", NearToken::from_near(5)).await;
    let before = s.usdc.balance_of(s.alice.id()).await;
    let started = now();

    let closed = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("the close lands");

    assert!(closed.confirmed);
    assert_eq!(closed.payout_account, s.alice.id().as_str());
    // A close proof is good for 3 hours.
    assert!(
        (started + 10_800..=now() + 10_800).contains(&closed.valid_until),
        "valid_until {} for a proof made at {started}",
        closed.valid_until
    );
    let commitment = entry(&s).commitment.clone();
    assert_eq!(status_of(&s, &commitment).await, "Closed");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    assert_eq!(entry(&s).status, NoteStatus::Closed);
    // The submitter key signed it: its nonce moved and the funding account's did not matter.
    let keys = sub.view_access_keys().await.unwrap();
    assert!(
        keys.iter().any(|k| k.access_key.nonce > 0),
        "the submitter's key signed a transaction"
    );
    // The wallet is archived: another close finds no note to close.
    assert!(matches!(
        s.gateway.close_note(0, no_delay(), None).await,
        Err(LeaveError::NoNote(0))
    ));
    assert!(s.closes.sent().is_empty(), "the operator was not used");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_submitter_key_can_call_nothing_else_on_the_vault() {
    let s = stack(Transport::Direct).await;
    let sub = submitter(&s, "personal", NearToken::from_near(5)).await;
    let made = s.gateway.submitters().get("personal").unwrap().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tx = nymira_operator::tx::NearTx::new(
        s.worker.rpc_addr(),
        sub.id().as_str(),
        s.vault.id().as_str(),
        made.key.clone(),
        Store::open(&dir.path().join("tx.db")).unwrap(),
        s.clock.clone(),
    )
    .with_config(tx_config());
    let end = tx
        .send(nymira_operator::tx::TxRequest {
            kind: "probe",
            method: "heartbeat",
            args: b"{}".to_vec(),
            gas: 30_000_000_000_000,
            idem_key: None,
        })
        .await;
    // The node refuses a method outside the key's list before the vault sees it.
    let refused = match end {
        Ok(e) => !e.success,
        Err(_) => true,
    };
    assert!(refused, "heartbeat is outside the key's methods");
    let text = nymirad::submitter::instructions(&made, s.vault.id().as_str());
    for m in METHODS {
        assert!(text.contains(m), "{m} is named in the instructions");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unused_clearance_still_allows_a_later_close_with_a_new_proof() {
    let s = stack(Transport::Direct).await;
    // A submitter with almost no NEAR cannot pay for gas.
    submitter(&s, "personal", NearToken::from_millinear(2)).await;

    let err = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect_err("no NEAR for gas");
    assert!(matches!(err, LeaveError::NoNear { .. }), "{err}");
    let commitment = entry(&s).commitment.clone();
    assert_eq!(status_of(&s, &commitment).await, "Active");

    // The operator's close takes it: the operator's closer is a stub here, so the vault does not
    // show the note closed and the answer says so.
    let sent = s
        .gateway
        .close_note(
            0,
            CloseOptions {
                via_operator: true,
                ..no_delay()
            },
            None,
        )
        .await
        .expect("the operator takes the close proof");
    assert!(!sent.confirmed);
    assert!(sent.tx_hash.starts_with("fakehash"));
    let handed = s.closes.sent();
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].1.len(), 256, "the proof goes out as made");

    // A new close proof from a funded submitter still works with the same state.
    s.gateway
        .submitters()
        .assign(&commitment, "second")
        .unwrap();
    submitter(&s, "second", NearToken::from_near(5)).await;
    let closed = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("a new close proof after an unused clearance");
    assert!(closed.confirmed);
    assert_eq!(status_of(&s, &commitment).await, "Closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_concedes_nothing_when_no_request_left_and_blocks_new_requests() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let commitment = entry(&s).commitment.clone();

    let exit = s
        .gateway
        .exit_note(0, None, None)
        .await
        .expect("exit filed");
    assert_eq!(exit.conceded, 0);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
    let rows: Value = s
        .vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(rows[0]["conceded"], "0");
    // The deadline is shown, seven days on.
    let chain_now = nymira_testkit::clock::now_seconds(&s.worker).await;
    assert!(exit.deadline_secs.abs_diff(chain_now + 7 * 86_400) < 60);
    assert_eq!(entry(&s).status, NoteStatus::Exiting);

    // No request on the note after the exit proof left.
    let refused = s
        .gateway
        .chat(
            json!({"model": MODEL, "messages": [{"role": "user", "content": "hi"}]}),
            ChatOptions::default(),
        )
        .await
        .err()
        .expect("a chat has no note to use");
    assert!(
        refused.message.contains("no accepted note"),
        "{}",
        refused.message
    );

    // A second `exit` reports the filing instead of filing again.
    let again = s.gateway.exit_note(0, None, None).await.unwrap();
    assert_eq!(again.deadline_secs, exit.deadline_secs);

    // Finalize waits for the deadline, on the local clock first.
    assert!(matches!(
        s.gateway.finalize_note(0, None, None).await,
        Err(LeaveError::NotYet { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn finalize_is_sent_by_the_submitter_after_the_deadline_and_the_vault_decides() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let exit = s.gateway.exit_note(0, None, None).await.unwrap();
    // The local clock is past the deadline, the sandbox's is not: the call is sent, and the vault
    // refuses it. (The sandbox cannot reach seven days; the vault's own tests cover the payout.)
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));
    let err = s
        .gateway
        .finalize_note(0, None, None)
        .await
        .expect_err("the vault's window is still open");
    assert!(
        err.to_string().contains("exit window is still open"),
        "{err}"
    );
    // While `nymirad` runs the same call is tried without asking, once a minute.
    let first = s.gateway.finalize_due().await;
    assert_eq!(first.len(), 1);
    assert!(first[0].1.is_err());
    assert!(
        s.gateway.finalize_due().await.is_empty(),
        "not again within a minute"
    );
    s.clock.advance_secs(61);
    assert_eq!(s.gateway.finalize_due().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_concedes_the_stored_bound_when_a_request_left_the_state() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    // A lease opens (a request proof leaves the device), then the process dies before it settles.
    let answer = s
        .gateway
        .chat(
            json!({"model": MODEL, "messages": [{"role": "user", "content": "hi"}]}),
            ChatOptions::default(),
        )
        .await
        .map_err(|e| e.message)
        .unwrap();
    drop(answer);
    let restarted = make_gateway(s.home.path(), &s.parts, Transport::Direct, s.clock.clone()).await;
    let exit = restarted
        .exit_note(0, None, None)
        .await
        .expect("exit filed");
    assert_eq!(exit.conceded, 100_000, "the 0.10 USDC bound class");
    let rows: Value = s
        .vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(rows[0]["conceded"], "100000");
    let _ = &s.op;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_tor_sends_nothing_unless_the_user_says_yes() {
    let socks = "127.0.0.1:9".parse().unwrap();
    let (tor, _state) = nymirad::tor::Tor::for_tests(
        socks,
        nymirad::tor::TorState::Failed("test: no circuit".into()),
    );
    let s = stack(Transport::Tor(Arc::new(tor))).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let commitment = entry(&s).commitment.clone();

    // Unattended: it waits, and asks nothing.
    assert!(matches!(
        s.gateway.exit_note(0, None, None).await,
        Err(LeaveError::TorDown(_))
    ));
    // Declined: nothing is sent, and the words are the fixed ones.
    let no = Scripted::new(false);
    assert!(matches!(
        s.gateway.exit_note(0, None, Some(&no)).await,
        Err(LeaveError::Declined)
    ));
    assert_eq!(*no.asked.lock().unwrap(), vec![direct_text(0, "0")]);
    assert_eq!(status_of(&s, &commitment).await, "Active");
    // Confirmed: the direct send goes out.
    let yes = Scripted::new(true);
    let exit = s
        .gateway
        .exit_note(0, None, Some(&yes))
        .await
        .expect("a confirmed direct send");
    assert_eq!(exit.conceded, 0);
    assert_eq!(yes.asked.lock().unwrap().len(), 1);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
    // A note that already left cannot be closed, and nothing is asked on the way: the clearance
    // and the operator's close never go direct, whatever the user says. (`tests/leave_gaps.rs`
    // covers a close on a note that is still open.)
    let close = s.gateway.close_note(0, no_delay(), Some(&yes)).await;
    assert!(matches!(close, Err(LeaveError::NoNote(0))), "{close:?}");
    assert_eq!(yes.asked.lock().unwrap().len(), 1, "no new question");
    assert!(s.closes.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_payout_is_collected_with_claim_payout() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let before = s.usdc.balance_of(s.alice.id()).await;
    // The token refuses transfers, so the close pays into the vault's pending payouts.
    s.usdc.set_paused(true).await;
    s.gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("the close lands even when the payout does not");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before);
    let pending: Value = s
        .vault
        .view("pending_payout")
        .args_json(json!({ "account_id": s.alice.id() }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(pending, ONE.to_string());

    s.usdc.set_paused(false).await;
    s.gateway
        .claim_payout(0, None, None)
        .await
        .expect("the payout is claimed");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    assert!(matches!(
        s.gateway.claim_payout(0, None, None).await,
        Err(LeaveError::NothingPending(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_reaches_close_over_the_owner_socket_and_not_over_http() {
    use nymirad::ownersock::{bind, request, serve, socket_path, Command};
    use std::os::unix::fs::PermissionsExt;

    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let listener = bind(s.home.path()).unwrap();
    let dir = socket_path(s.home.path()).parent().unwrap().to_path_buf();
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(socket_path(s.home.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tokio::spawn(serve(listener, s.gateway.clone()));

    let mut said = Vec::new();
    let text = request(
        s.home.path(),
        &Command::Close {
            note: 0,
            delay_secs: Some(0),
            family: None,
            via_operator: false,
        },
        |l| said.push(l.to_string()),
        |_| false,
    )
    .await
    .expect("the close runs over the socket");
    assert!(text.contains("is closed"), "{text}");
    assert!(said.iter().any(|l| l.contains("on its own Tor circuit")));

    // A group- or world-readable directory is refused.
    let loose = tempfile::tempdir().unwrap();
    std::fs::create_dir(loose.path().join("run")).unwrap();
    std::fs::set_permissions(
        loose.path().join("run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(bind(loose.path()).is_err());

    // None of the owner commands is on the HTTP route table.
    for route in ["/v1/close", "/v1/exit", "/v1/finalize", "/v1/claim-payout"] {
        assert!(
            !nymirad::guard::OWNER_ROUTES
                .iter()
                .any(|(_, p)| *p == route),
            "{route}"
        );
    }
}
