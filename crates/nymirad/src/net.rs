//! How nymirad's traffic leaves the machine: identities for Tor circuits and the reading of
//! failures, so "Tor is down" and "the other side refuses Tor" are never mixed up.

use std::error::Error;

/// A random 32-byte SOCKS identity, hex. With `IsolateSOCKSAuth` Tor keeps connections that
/// carry different identities on different circuits. Tor exits can repeat, so this does not
/// stop a timing or usage link between leases; it stops circuit sharing.
pub fn identity() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// How many circuits a fetch tries before it gives up: some destinations (NVIDIA's attestation
/// service answers 403) turn away most Tor exits and let others through.
pub const CIRCUIT_TRIES: u32 = 4;

/// Runs `attempt` with a fresh identity (so a fresh circuit) until it succeeds or `tries` attempts
/// have failed; returns the last error. `attempt` gets the identity and the attempt number from 0.
pub async fn on_fresh_circuits<T, E, F, Fut>(tries: u32, mut attempt: F) -> Result<T, E>
where
    F: FnMut(String, u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut n = 0;
    loop {
        match attempt(identity(), n).await {
            Ok(v) => return Ok(v),
            Err(e) if n + 1 >= tries => return Err(e),
            Err(_) => n += 1,
        }
    }
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

/// Opens a SOCKS5 connection to `host:port` under `identity` and closes it without sending a
/// byte. Tor builds the circuit for that identity on the first stream, so the request that
/// follows on a client with the same identity does not wait for one. Best effort: an error only
/// means the circuit is built later.
pub async fn prewarm(socks: &str, identity: &str, host: &str, port: u16) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    if host.len() > 255 || identity.len() > 255 {
        return Err(bad("name too long"));
    }
    let mut s = tokio::net::TcpStream::connect(socks).await?;
    s.write_all(&[5, 1, 2]).await?;
    let mut method = [0u8; 2];
    s.read_exact(&mut method).await?;
    if method != [5, 2] {
        return Err(bad("the proxy wants no user/password"));
    }
    let mut auth = vec![1, identity.len() as u8];
    auth.extend(identity.as_bytes());
    auth.extend([1, b'x']);
    s.write_all(&auth).await?;
    let mut ok = [0u8; 2];
    s.read_exact(&mut ok).await?;
    if ok[1] != 0 {
        return Err(bad("the proxy refused the identity"));
    }
    let mut connect = vec![5, 1, 0];
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            connect.push(1);
            connect.extend(ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            connect.push(4);
            connect.extend(ip.octets());
        }
        // A name is left to the proxy to resolve.
        Err(_) => {
            connect.push(3);
            connect.push(host.len() as u8);
            connect.extend(host.as_bytes());
        }
    }
    connect.extend(port.to_be_bytes());
    s.write_all(&connect).await?;
    let mut reply = [0u8; 4];
    s.read_exact(&mut reply).await?;
    if reply[1] != 0 {
        return Err(bad("the proxy could not connect"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failing_fetch_is_retried_on_new_identities_and_then_gives_up() {
        let mut seen = Vec::new();
        let ok: Result<u32, String> = on_fresh_circuits(4, |id, n| {
            seen.push(id);
            async move {
                if n < 2 {
                    Err(format!("403 on try {n}"))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(ok, Ok(2));
        assert_eq!(seen.len(), 3);
        assert_eq!(
            seen.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );

        let mut count = 0;
        let bad: Result<(), String> = on_fresh_circuits(4, |_, n| {
            count += 1;
            async move { Err(format!("try {n}")) }
        })
        .await;
        assert_eq!(bad, Err("try 3".to_string()));
        assert_eq!(count, 4);
    }
}
