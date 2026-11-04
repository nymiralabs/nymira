//! Live 1Click `dry: true` quotes for a short token shortlist, shown on a testnet build as
//! "quote only". The cross-chain payment itself is never run on testnet.
//!
//! Requests carry a dummy refund and recipient address and no note data, go through the Tor
//! proxy only, and are spaced at least five seconds apart (1Click's keyless limit). If 1Click
//! refuses Tor exits, the display is switched off, never sent directly.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nymira_operator::clock::Clock;
use serde_json::Value;

use crate::mode::Mode;
use crate::rpc::{client, Transport};

pub const BASE_URL: &str = "https://1click.chaindefuser.com";
/// Least gap between two requests.
pub const MIN_GAP_SECS: u64 = 5;
/// A quote older than this is asked for again.
pub const TTL_SECS: u64 = 180;
pub const LABEL: &str = "quote only";
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

/// 1Click over Tor. There is no constructor for a direct client.
pub struct OneClick {
    route: Route,
    base: String,
    amount: u128,
    mock_key: bool,
}

enum Route {
    /// A fresh connection and identity for every quote.
    Transport(Transport),
    /// A client the test built.
    #[cfg(feature = "mock")]
    Client(reqwest::Client),
}

impl OneClick {
    pub fn new(base: &str, transport: &Transport) -> Result<Self, String> {
        if base != BASE_URL && !cfg!(feature = "mock") {
            return Err("1Click base URL is compiled into this build".into());
        }
        if transport.is_direct() {
            return Err("1Click is reached through Tor only".into());
        }
        Ok(Self {
            route: Route::Transport(transport.clone()),
            base: base.trim_end_matches('/').into(),
            amount: 1_050_000,
            mock_key: false,
        })
    }

    /// Over a client the caller built, for a stub on loopback in tests. The shipped path is
    /// [`OneClick::new`].
    #[doc(hidden)]
    #[cfg(feature = "mock")]
    pub fn over(http: reqwest::Client, base: &str) -> Self {
        Self {
            route: Route::Client(http),
            base: base.trim_end_matches('/').into(),
            amount: 1_050_000,
            mock_key: true,
        }
    }
    pub fn with_amount(mut self, amount: u128) -> Self {
        self.amount = amount;
        self
    }
}

#[async_trait]
impl QuoteSource for OneClick {
    async fn quote(&self, asset: &Asset) -> Result<String, QuoteError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| QuoteError::Failed(e.to_string()))?
            .as_secs();
        let body = crate::intents::request(
            asset.id,
            self.amount,
            crate::intents::PLACEHOLDER,
            &crate::intents::placeholder_message(),
            crate::intents::DUMMY,
            true,
            now,
        );
        let http = match &self.route {
            #[cfg(feature = "mock")]
            Route::Client(c) => c.clone(),
            Route::Transport(t) => client(t).map_err(|e| QuoteError::Failed(e.to_string()))?,
        };
        let reply = http
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
        let verified = crate::oneclick::verify_signature(&value);
        #[cfg(feature = "mock")]
        let verified = if self.mock_key {
            crate::oneclick::verify_mock_signature(&value)
        } else {
            verified
        };
        let _ = self.mock_key;
        verified.map_err(QuoteError::Failed)?;
        crate::oneclick::validate_quote(&value, &body, now).map_err(QuoteError::Failed)?;
        value["quote"]["amountInFormatted"]
            .as_str()
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
                Err(_) => {
                    self.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .lines
                        .retain(|l| l.label != asset.label);
                }
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
