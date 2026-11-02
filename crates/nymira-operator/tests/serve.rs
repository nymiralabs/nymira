//! The public listener's limits, on a real TCP socket.

use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use nymira_operator::serve::{raise_fd_limit, serve, Limits, FD_LIMIT};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn app() -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route(
            "/panic",
            get(|| async {
                if true {
                    panic!("secret prompt text");
                }
                "unreachable"
            }),
        )
        .route(
            "/echo",
            post(|body: axum::body::Body| async move {
                match to_bytes(body, usize::MAX).await {
                    Ok(bytes) => (StatusCode::OK, bytes.len().to_string()),
                    Err(_) => (StatusCode::REQUEST_TIMEOUT, "slow".into()),
                }
            }),
        )
}

async fn start(limits: Limits) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, app(), limits));
    addr
}

fn quick() -> Limits {
    Limits {
        header_timeout: Duration::from_millis(400),
        body_min_rate: 1024,
        body_grace: Duration::from_millis(500),
        max_connections: 512,
    }
}

/// True when the peer closed the connection within `within`.
async fn closed_within(stream: &mut TcpStream, within: Duration) -> bool {
    let mut buf = [0u8; 512];
    let started = Instant::now();
    while started.elapsed() < within {
        match tokio::time::timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return true,
            _ => {}
        }
    }
    false
}

#[tokio::test]
async fn a_slow_header_client_is_cut_off() {
    let addr = start(quick()).await;
    let mut slow = TcpStream::connect(addr).await.unwrap();
    slow.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let started = Instant::now();
    // The header never ends; one more byte every 100 ms does not extend the deadline.
    let mut cut = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if slow.write_all(b"X").await.is_err() {
            cut = true;
            break;
        }
        if closed_within(&mut slow, Duration::from_millis(1)).await {
            cut = true;
            break;
        }
    }
    assert!(cut, "the connection was still open after 3 seconds");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_slow_body_client_is_cut_off_and_a_fast_one_is_not() {
    let addr = start(quick()).await;
    let mut slow = TcpStream::connect(addr).await.unwrap();
    slow.write_all(b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 100000\r\n\r\n")
        .await
        .unwrap();
    let started = Instant::now();
    let mut answer = Vec::new();
    for _ in 0..40 {
        if slow.write_all(b"a").await.is_err() {
            break;
        }
        let mut buf = [0u8; 512];
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(100), slow.read(&mut buf)).await
        {
            answer.extend_from_slice(&buf[..n]);
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(3), "not cut off");
    assert!(
        String::from_utf8_lossy(&answer).contains("408") || answer.is_empty(),
        "unexpected answer: {}",
        String::from_utf8_lossy(&answer)
    );

    let mut fast = TcpStream::connect(addr).await.unwrap();
    let body = vec![b'b'; 20_000];
    fast.write_all(
        format!(
            "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    fast.write_all(&body).await.unwrap();
    let mut text = String::new();
    fast.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("20000"), "{text}");
}

#[tokio::test]
async fn a_burst_of_idle_connections_stops_at_the_cap() {
    let addr = start(Limits {
        header_timeout: Duration::from_secs(30),
        max_connections: 8,
        ..quick()
    })
    .await;
    let mut held = Vec::new();
    for _ in 0..40 {
        held.push(TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut dropped = 0;
    for stream in &mut held {
        if closed_within(stream, Duration::from_millis(150)).await {
            dropped += 1;
        }
    }
    assert_eq!(dropped, 32, "8 connections stay, the other 32 are refused");
    // A freed slot takes a new connection.
    drop(held);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut again = TcpStream::connect(addr).await.unwrap();
    again
        .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut text = String::new();
    again.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
}

#[tokio::test]
async fn a_panic_answers_a_fixed_500_and_the_service_goes_on() {
    let addr = start(quick()).await;
    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(b"GET /panic HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 500"), "{text}");
    assert!(!text.contains("secret prompt text"));
    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
}

#[tokio::test]
async fn a_body_over_the_cap_is_refused_before_the_handler() {
    let addr = start(quick()).await;
    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(
        b"POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut text = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.read_to_string(&mut text)).await;
    assert!(text.starts_with("HTTP/1.1 413"), "{text}");
}

#[test]
fn the_descriptor_limit_is_raised() {
    let now = raise_fd_limit(FD_LIMIT).unwrap();
    assert!(now >= FD_LIMIT.min(1024), "limit is {now}");
}
