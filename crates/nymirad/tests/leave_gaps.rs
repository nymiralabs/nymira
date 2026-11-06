//! Gaps in the close, exit, finalize and claim paths: the default random close delay, a dead Tor
//! with a direct-send question for every sender, an exit after a clearance that was never used,
//! and the same commands over the owner socket. Runs against the sandbox stack of `close.rs`.

#[path = "support/socks.rs"]
mod socks;
#[path = "support/stack.rs"]
mod stack;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use near_workspaces::types::NearToken;
use nymira_operator::clock::Clock;
use nymirad::gateway::close::{direct_text, Ask, CloseOptions, LeaveError, MAX_CLOSE_DELAY_SECS};
use nymirad::ownersock::{bind, request, serve, Command};
use nymirad::rpc::Transport;
use nymirad::secrets::NoteStatus;
use nymirad::tor::{Tor, TorState};
use socks::{Behavior, FakeSocks};
use stack::*;

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
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl Ask for Scripted {
    async fn confirm_direct(&self, text: &str) -> bool {
        self.asked.lock().unwrap().push(text.into());
        self.yes
    }
}

/// A stack whose Tor is dead: every send either waits, asks, or fails.
async fn dead_tor_stack() -> Stack {
    let (tor, _state) = Tor::for_tests(
        "127.0.0.1:9".parse().unwrap(),
        TorState::Failed("test: no circuit".into()),
    );
    let s = stack(Transport::Tor(Arc::new(tor))).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    s
}

fn is_identity(user: &str) -> bool {
    user.len() == 64 && user.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---- #33: the default close delay -------------------------------------------------------------

#[test]
fn the_default_close_delay_is_random_and_inside_its_bounds() {
    let delays: Vec<u64> = (0..300)
        .map(|_| CloseOptions::default().delay.as_secs())
        .collect();
    assert!(
        delays
            .iter()
            .all(|d| (60..MAX_CLOSE_DELAY_SECS).contains(d)),
        "outside 60..{MAX_CLOSE_DELAY_SECS}: {delays:?}"
    );
    let distinct: std::collections::BTreeSet<_> = delays.iter().collect();
    assert!(
        distinct.len() > 100,
        "{} distinct values in 300",
        distinct.len()
    );
    // The ownership proof lives 55 minutes: the longest delay leaves it room.
    const _: () = assert!(MAX_CLOSE_DELAY_SECS < 55 * 60);
    // `no_delay()` is the only way tests skip it.
    assert!(no_delay().delay.is_zero());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_waits_its_delay_before_the_proof_leaves_and_uses_an_identity_of_its_own() {
    // Tor is a recording proxy in front of loopback, so what the close sends is visible.
    let proxy = FakeSocks::start(Behavior::Passthrough).await;
    let (tor, _state) = Tor::for_tests(proxy.addr, TorState::Ready);
    let s = stack(Transport::Tor(Arc::new(tor))).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let operator_port: u16 = s
        .parts
        .operator_url
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let delay = Duration::from_millis(1500);
    let opts = CloseOptions {
        delay,
        ..no_delay()
    };

    let started = Instant::now();
    let gateway = s.gateway.clone();
    let close = tokio::spawn(async move { gateway.close_note(0, opts, None).await });
    tokio::time::sleep(delay / 3).await;
    assert!(
        !proxy.seen().iter().any(|c| c.port == operator_port),
        "the clearance request left before the delay passed"
    );
    let closed = close.await.unwrap().expect("the close lands");
    assert!(closed.confirmed);
    assert!(started.elapsed() >= delay, "{:?}", started.elapsed());

    let seen = proxy.seen();
    let to_operator: Vec<_> = seen.iter().filter(|c| c.port == operator_port).collect();
    assert_eq!(to_operator.len(), 1, "one connection carries the clearance");
    let own = &to_operator[0].user;
    assert!(is_identity(own), "{own}");
    // Nothing else the close sent shares that Tor identity (circuit).
    assert!(
        seen.iter()
            .filter(|c| c.port != operator_port)
            .all(|c| c.user != *own),
        "the clearance shares a circuit with another request: {seen:?}"
    );
}

// ---- dead Tor: close, finalize, claim, finalize_due -------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_close_with_a_dead_tor_stops_before_anything_is_sent_and_never_asks() {
    let s = dead_tor_stack().await;
    let commitment = entry(&s).commitment.clone();
    let yes = Scripted::new(true);

    let err = s
        .gateway
        .close_note(0, no_delay(), Some(&yes))
        .await
        .expect_err("a close needs Tor");
    assert!(matches!(err, LeaveError::TorDown(_)), "{err}");
    assert!(yes.asked().is_empty(), "the clearance never goes direct");
    assert!(s.closes.sent().is_empty());
    assert_eq!(status_of(&s, &commitment).await, "Active");
    assert_eq!(entry(&s).status, NoteStatus::Accepted);
    // Through the operator it is the same.
    let via_operator = CloseOptions {
        via_operator: true,
        ..no_delay()
    };
    assert!(matches!(
        s.gateway.close_note(0, via_operator, Some(&yes)).await,
        Err(LeaveError::TorDown(_))
    ));
    assert!(s.closes.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn finalize_with_a_dead_tor_waits_asks_and_goes_direct_only_on_yes() {
    let s = dead_tor_stack().await;
    let yes = Scripted::new(true);
    let exit = s.gateway.exit_note(0, None, Some(&yes)).await.unwrap();
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));

    // Unattended: waits, asks nothing.
    assert!(matches!(
        s.gateway.finalize_note(0, None, None).await,
        Err(LeaveError::TorDown(_))
    ));
    // Declined: the fixed question, nothing sent.
    let no = Scripted::new(false);
    assert!(matches!(
        s.gateway.finalize_note(0, None, Some(&no)).await,
        Err(LeaveError::Declined)
    ));
    assert_eq!(no.asked(), vec![direct_text(0, "0")]);
    // Confirmed: the call reaches the vault (which refuses it, the sandbox cannot wait 7 days).
    let yes = Scripted::new(true);
    let err = s
        .gateway
        .finalize_note(0, None, Some(&yes))
        .await
        .expect_err("the window is still open on chain");
    assert!(
        err.to_string().contains("exit window is still open"),
        "{err}"
    );
    assert_eq!(yes.asked(), vec![direct_text(0, "0")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn finalize_due_never_goes_direct_when_tor_is_dead() {
    let s = dead_tor_stack().await;
    let yes = Scripted::new(true);
    let exit = s.gateway.exit_note(0, None, Some(&yes)).await.unwrap();
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));

    let tried = s.gateway.finalize_due().await;
    assert_eq!(tried.len(), 1);
    // A direct send would have reached the vault and been refused for its open window; the
    // answer is the dead Tor, so nothing left.
    assert!(
        matches!(tried[0].1, Err(LeaveError::TorDown(_))),
        "{:?}",
        tried[0].1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_payout_with_a_dead_tor_waits_asks_and_goes_direct_only_on_yes() {
    let s = dead_tor_stack().await;
    assert!(matches!(
        s.gateway.claim_payout(0, None, None).await,
        Err(LeaveError::TorDown(_))
    ));
    let no = Scripted::new(false);
    assert!(matches!(
        s.gateway.claim_payout(0, None, Some(&no)).await,
        Err(LeaveError::Declined)
    ));
    assert_eq!(no.asked(), vec![direct_text(0, "not recorded")]);
    // Confirmed: the direct view reaches the vault, which has nothing pending for the payout.
    let yes = Scripted::new(true);
    let err = s
        .gateway
        .claim_payout(0, None, Some(&yes))
        .await
        .expect_err("nothing is pending");
    assert!(matches!(err, LeaveError::NothingPending(_)), "{err}");
    assert_eq!(yes.asked(), vec![direct_text(0, "not recorded")]);
}

// ---- exit after a clearance that was not used -------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_works_after_a_clearance_was_issued_and_the_close_never_went_out() {
    let s = stack(Transport::Direct).await;
    // No NEAR for gas: the clearance is issued and the wallet is locked for a mutual close, then
    // the send stops.
    submitter(&s, "personal", NearToken::from_millinear(2)).await;
    let commitment = entry(&s).commitment.clone();
    let err = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect_err("no NEAR");
    assert!(matches!(err, LeaveError::NoNear { .. }), "{err}");
    assert_eq!(status_of(&s, &commitment).await, "Active");

    s.gateway
        .submitters()
        .assign(&commitment, "second")
        .unwrap();
    submitter(&s, "second", NearToken::from_near(5)).await;
    let exit = s
        .gateway
        .exit_note(0, None, None)
        .await
        .expect("the exit is not blocked by the earlier clearance");
    assert_eq!(exit.conceded, 0);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
    assert_eq!(entry(&s).status, NoteStatus::Exiting);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_works_after_the_operator_took_a_close_the_vault_never_showed() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let commitment = entry(&s).commitment.clone();
    // The operator's closer is a stub: the close is handed over and the note stays open.
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
        .expect("the operator takes the close");
    assert!(!sent.confirmed);
    assert_eq!(s.closes.sent().len(), 1);
    assert_eq!(status_of(&s, &commitment).await, "Active");

    let exit = s
        .gateway
        .exit_note(0, None, None)
        .await
        .expect("an exit after a close the chain never saw");
    assert_eq!(exit.conceded, 0);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
}

// ---- the owner socket -------------------------------------------------------------------------

async fn socket(s: &Stack) {
    let listener = bind(s.home.path()).unwrap();
    tokio::spawn(serve(listener, s.gateway.clone()));
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_and_finalize_run_over_the_owner_socket() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    socket(&s).await;
    let commitment = entry(&s).commitment.clone();

    let text = request(
        s.home.path(),
        &Command::Exit {
            note: 0,
            family: None,
        },
        |_| {},
        |_| false,
    )
    .await
    .expect("the exit runs over the socket");
    assert!(
        text.contains("Exit filed for note 0 (conceded 0)"),
        "{text}"
    );
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");

    // Before the deadline the socket reports the wait, not a success.
    let early = request(
        s.home.path(),
        &Command::Finalize {
            note: 0,
            family: None,
        },
        |_| {},
        |_| false,
    )
    .await
    .expect_err("the window is open");
    assert!(early.contains("exit window of note 0 is open"), "{early}");

    // After it, the call reaches the vault, which still refuses (the sandbox cannot wait 7 days).
    let exit = s.gateway.exit_note(0, None, None).await.unwrap();
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));
    let late = request(
        s.home.path(),
        &Command::Finalize {
            note: 0,
            family: None,
        },
        |_| {},
        |_| false,
    )
    .await
    .expect_err("the vault refuses");
    assert!(late.contains("exit window is still open"), "{late}");
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_payout_runs_over_the_owner_socket() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    socket(&s).await;
    let before = s.usdc.balance_of(s.alice.id()).await;
    s.usdc.set_paused(true).await;
    s.gateway.close_note(0, no_delay(), None).await.unwrap();
    s.usdc.set_paused(false).await;

    let claim = Command::ClaimPayout {
        note: 0,
        family: None,
    };
    let text = request(s.home.path(), &claim, |_| {}, |_| false)
        .await
        .expect("the claim runs over the socket");
    assert!(text.contains("Payout of note 0 claimed"), "{text}");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    let again = request(s.home.path(), &claim, |_| {}, |_| false)
        .await
        .expect_err("nothing is left to claim");
    assert!(again.contains("nothing is pending"), "{again}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_direct_send_question_travels_over_the_owner_socket_and_the_answer_decides() {
    let s = dead_tor_stack().await;
    socket(&s).await;
    let commitment = entry(&s).commitment.clone();
    let exit = Command::Exit {
        note: 0,
        family: None,
    };

    let mut asked = Vec::new();
    let declined = request(
        s.home.path(),
        &exit,
        |_| {},
        |q| {
            asked.push(q.to_string());
            false
        },
    )
    .await
    .expect_err("declined");
    assert!(
        declined.contains("direct send was not confirmed"),
        "{declined}"
    );
    assert_eq!(asked, vec![direct_text(0, "0")]);
    assert_eq!(status_of(&s, &commitment).await, "Active");

    let mut asked = Vec::new();
    let text = request(
        s.home.path(),
        &exit,
        |_| {},
        |q| {
            asked.push(q.to_string());
            true
        },
    )
    .await
    .expect("confirmed");
    assert!(text.contains("Exit filed for note 0"), "{text}");
    assert_eq!(asked, vec![direct_text(0, "0")]);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
}
