use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use nymira_operator::clock::ManualClock;
use nymirad::mode::Mode;
use nymirad::quotes::{Asset, Board, QuoteError, QuoteSource, View, LABEL, MIN_GAP_SECS, TTL_SECS};

struct Fake {
    calls: AtomicU32,
    refuse: bool,
}

#[async_trait]
impl QuoteSource for Fake {
    async fn quote(&self, _: &Asset) -> Result<String, QuoteError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            Err(QuoteError::TorRefused)
        } else {
            Ok("0.99".into())
        }
    }
}

fn board(mode: Mode, refuse: bool, tor: bool) -> (Board, Arc<Fake>, Arc<ManualClock>) {
    let source = Arc::new(Fake {
        calls: AtomicU32::new(0),
        refuse,
    });
    let clock = Arc::new(ManualClock::new(1_900_000_000_000));
    let src: Option<Arc<dyn QuoteSource>> = tor.then(|| source.clone() as Arc<dyn QuoteSource>);
    (Board::new(mode, src, clock.clone()), source, clock)
}

#[tokio::test]
async fn quotes_are_cached_and_asked_for_no_faster_than_the_limit() {
    let (board, source, clock) = board(Mode::Testnet, false, true);
    for _ in 0..10 {
        let View::Lines(lines) = board.view().await else {
            panic!("off")
        };
        assert_eq!(lines[0].text, "0.99");
    }
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "one fetch serves the cache"
    );
    clock.advance_secs(TTL_SECS);
    board.view().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    assert!(MIN_GAP_SECS >= 5 && LABEL == "quote only");
}

#[tokio::test]
async fn the_gap_between_two_requests_holds_even_when_the_cache_is_stale() {
    let (board, source, clock) = board(Mode::Testnet, false, true);
    board.view().await;
    clock.advance_secs(TTL_SECS - 1);
    board.view().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    // The next request after the entry went stale is at least the gap after the last one.
    clock.advance_secs(1);
    board.view().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn quotes_are_off_without_tor_off_on_mainnet_and_off_when_tor_exits_are_refused() {
    let (no_tor, source, _) = board(Mode::Testnet, false, false);
    assert!(matches!(no_tor.view().await, View::Off(_)));
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    let (main, source, _) = board(Mode::Mainnet, false, true);
    assert!(matches!(main.view().await, View::Off(_)));
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    let (refused, source, clock) = board(Mode::Testnet, true, true);
    assert!(matches!(refused.view().await, View::Off(_)));
    clock.advance_secs(3_600);
    assert!(matches!(refused.view().await, View::Off(_)));
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "no retry once refused"
    );
}

#[test]
fn the_intents_paths_are_a_mainnet_profile_feature() {
    assert!(!Mode::Testnet.intents_enabled());
    assert!(Mode::Mainnet.intents_enabled());
    assert!(Mode::parse("mock").is_err());
    assert_eq!(Mode::parse("testnet").unwrap(), Mode::Testnet);
}
