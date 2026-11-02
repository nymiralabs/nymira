//! Live 1Click `dry: true` quotes for a short token shortlist, shown on a testnet build as
//! "quote only". The cross-chain payment itself is never run on testnet.
//!
//! Requests carry a dummy refund and recipient address and no note data, go through the Tor
//! proxy only, and are spaced at least five seconds apart (1Click's keyless limit). If 1Click
//! refuses Tor exits, the display is switched off, never sent directly.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nymira_operator::clock::Clock;
use serde_json::{json, Value};

use crate::mode::Mode;
use crate::rpc::{client, Transport};

pub const BASE_URL: &str = "https://1click.chaindefuser.com";
/// Least gap between two requests.
pub const MIN_GAP_SECS: u64 = 5;
/// A quote older than this is asked for again.
pub const TTL_SECS: u64 = 60;
pub const LABEL: &str = "quote only";
/// A well-formed address that belongs to nobody; nothing about the user goes in a quote.
const DUMMY_ADDRESS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// NEAR USDC, the destination of every shortlisted quote.
const DESTINATION: &str = "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub label: &'static str,
    pub id: &'static str,
}

/// The shortlist: USDC on Ethereum.
pub const SHORTLIST: [Asset; 1] = [Asset {
    label: "USDC on Ethereum",
    id: "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near",
}];

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum QuoteError {
    /// 1Click answered but will not serve Tor exits.
    #[error("1Click refuses Tor exits")]
    TorRefused,
    #[error("quote failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait QuoteSource: Send + Sync {
    /// The amount out for 1 USDC in, as the text 1Click returns.
    async fn quote(&self, asset: &Asset) -> Result<String, QuoteError>;
}

/// 1Click over a SOCKS proxy. There is no constructor without one.
pub struct OneClick {
    http: reqwest::Client,
    base: String,
}

impl OneClick {
    pub fn new(base: &str, socks_addr: &str) -> Result<Self, String> {
        Ok(Self {
            http: client(&Transport::Socks(socks_addr.into())).map_err(|e| e.to_string())?,
            base: base.trim_end_matches('/').into(),
        })
    }
}

#[async_trait]
impl QuoteSource for OneClick {
    async fn quote(&self, asset: &Asset) -> Result<String, QuoteError> {
        let body = json!({
            "dry": true,
            "swapType": "EXACT_INPUT",
            "slippageTolerance": 100,
            "originAsset": asset.id,
            "depositType": "ORIGIN_CHAIN",
            "destinationAsset": DESTINATION,
            "amount": "1000000",
            "refundTo": DUMMY_ADDRESS,
            "refundType": "ORIGIN_CHAIN",
            "recipient": DUMMY_ADDRESS,
            "recipientType": "DESTINATION_CHAIN",
            "deadline": "2099-01-01T00:00:00.000Z",
        });
        let reply = self
            .http
            .post(format!("{}/v0/quote", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| QuoteError::Failed(e.without_url().to_string()))?;
        match reply.status().as_u16() {
            403 | 451 => return Err(QuoteError::TorRefused),
            s if !(200..300).contains(&s) => return Err(QuoteError::Failed(format!("status {s}"))),
            _ => {}
        }
        let value: Value = reply
            .json()
            .await
            .map_err(|e| QuoteError::Failed(e.without_url().to_string()))?;
        value["quote"]["amountOutFormatted"]
            .as_str()
            .or_else(|| value["quote"]["amountOut"].as_str())
            .map(String::from)
            .ok_or_else(|| QuoteError::Failed("no amount in the quote".into()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub label: &'static str,
    /// What 1 USDC buys, `quote only`.
    pub text: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Not shown: not a testnet build, or no Tor, or 1Click refused Tor exits.
    Off(&'static str),
    Lines(Vec<Line>),
}

#[derive(Default)]
struct State {
    lines: Vec<Line>,
    last_request: Option<u64>,
    refused: bool,
}

pub struct Board {
    mode: Mode,
    source: Option<Arc<dyn QuoteSource>>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl Board {
    /// `source` is `None` when no Tor proxy is configured: the board then shows nothing.
    pub fn new(mode: Mode, source: Option<Arc<dyn QuoteSource>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            mode,
            source,
            clock,
            state: Mutex::new(State::default()),
        }
    }

    /// The lines to show, asking 1Click for at most one stale entry per call and never sooner
    /// than the gap after the last request.
    pub async fn view(&self) -> View {
        if self.mode != Mode::Testnet {
            return View::Off("quotes are shown on testnet builds only");
        }
        let Some(source) = &self.source else {
            return View::Off("quotes need Tor, which is not configured");
        };
        if self.state.lock().unwrap_or_else(|e| e.into_inner()).refused {
            return View::Off("1Click refuses Tor exits; quotes are off");
        }
        let now = self.clock.now_secs();
        let next = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let gap_ok = state
                .last_request
                .is_none_or(|at| now.saturating_sub(at) >= MIN_GAP_SECS);
            SHORTLIST
                .iter()
                .find(|asset| {
                    state
                        .lines
                        .iter()
                        .find(|l| l.label == asset.label)
                        .is_none_or(|l| now.saturating_sub(l.at) >= TTL_SECS)
                })
                .filter(|_| gap_ok)
        };
        if let Some(asset) = next {
            // The slot is taken before the request goes out, so concurrent callers wait.
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last_request = Some(now);
            match source.quote(asset).await {
                Ok(text) => {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.lines.retain(|l| l.label != asset.label);
                    state.lines.push(Line {
                        label: asset.label,
                        text,
                        at: now,
                    });
                }
                Err(QuoteError::TorRefused) => {
                    self.state.lock().unwrap_or_else(|e| e.into_inner()).refused = true;
                    return View::Off("1Click refuses Tor exits; quotes are off");
                }
                Err(_) => {}
            }
        }
        View::Lines(
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .lines
                .clone(),
        )
    }
}
