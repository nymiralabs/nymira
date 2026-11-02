//! How nymirad's traffic leaves the machine: identities for Tor circuits and the reading of
//! failures, so "Tor is down" and "the other side refuses Tor" are never mixed up.

use std::error::Error;

/// A random 32-byte SOCKS identity, hex. With `IsolateSOCKSAuth` Tor keeps connections that
/// carry different identities on different circuits. Tor exits can repeat, so this does not
/// stop a timing or usage link between leases; it stops circuit sharing.
pub fn identity() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The proxy is not there: Tor is stopped or not listening.
    TorDown(String),
    /// Tor answered and could not reach, or was not let through to, the destination.
    TorRefused(String),
    Other(String),
}

/// The text of an error and everything under it.
fn chain(e: &(dyn Error + 'static)) -> String {
    let mut parts = vec![e.to_string()];
    let mut next = e.source();
    while let Some(inner) = next {
        parts.push(inner.to_string());
        next = inner.source();
    }
    parts.join(": ")
}

/// Reads a failed send. Only the shape of the error is used, so this holds for any caller.
pub fn classify(e: reqwest::Error) -> Failure {
    let text = chain(&e).to_lowercase();
    let connect = e.is_connect();
    let message = e.without_url().to_string();
    if connect && text.contains("socks") {
        // The client could not open the connection to the proxy, or the proxy went away in the
        // handshake: Tor is down. Any other SOCKS error is Tor's answer about the destination
        // (not allowed, refused, unreachable, general failure).
        let proxy_gone = [
            "underlying connection",
            "eof",
            "reset",
            "broken pipe",
            "io error",
        ]
        .iter()
        .any(|m| text.contains(m));
        return if proxy_gone {
            Failure::TorDown(message)
        } else {
            Failure::TorRefused(message)
        };
    }
    Failure::Other(message)
}

/// A status that says the destination turns Tor exits away.
pub fn refuses_tor(status: u16) -> bool {
    matches!(status, 403 | 451)
}
