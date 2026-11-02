//! The public listener: limits that need no client identity.
//!
//! A header read timeout, a minimum body rate, a global connection cap, one request at a time
//! per connection (HTTP/1.1 only), a body size cap before any handler runs and a panic in a
//! handler answered with a fixed 500. Nothing here sees or keeps an IP address.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use http_body::{Frame, SizeHint};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::{Instant, Interval};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;

/// Open files the public process asks for.
pub const FD_LIMIT: u64 = 4096;
/// No route takes a body above this (the chat route's 4 MiB is the largest).
pub const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Time to receive a request's headers, from the first byte; it also bounds how long an
    /// idle keep-alive connection waits for its next request.
    pub header_timeout: Duration,
    /// A body must arrive at this many bytes per second, once `body_grace` has passed.
    pub body_min_rate: u64,
    pub body_grace: Duration,
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(10),
            body_min_rate: 1024,
            body_grace: Duration::from_secs(5),
            max_connections: 512,
        }
    }
}

/// Raises the open-file limit to `want`, or to the hard limit if that is lower. Returns the
/// limit in force.
pub fn raise_fd_limit(want: u64) -> std::io::Result<u64> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscalls with a valid pointer.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let target = want.min(lim.rlim_max);
        if lim.rlim_cur < target {
            lim.rlim_cur = target as libc::rlim_t;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(lim.rlim_cur)
}

/// Serves `app` on `listener` until the task is dropped.
pub async fn serve(listener: TcpListener, app: Router, limits: Limits) {
    let app = app
        .layer(middleware::from_fn(move |req, next| {
            min_rate(req, next, limits)
        }))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .layer(CatchPanicLayer::custom(|_| {
            // A fixed answer: the panic message may hold request data.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "code": "internal", "message": "internal error" })),
            )
                .into_response()
        }));
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of descriptors or a dying socket: wait, do not spin.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_timeout)
                .keep_alive(true)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

async fn min_rate(request: Request<Body>, next: Next, limits: Limits) -> Response {
    let (parts, body) = request.into_parts();
    let body = Body::new(MinRate::new(body, limits));
    next.run(Request::from_parts(parts, body)).await
}

/// A body that fails once it has run slower than the minimum rate, so a client that sends a byte
/// at a time cannot hold a handler open.
struct MinRate {
    inner: Body,
    started: Instant,
    received: u64,
    limits: Limits,
    tick: Interval,
}

impl MinRate {
    fn new(inner: Body, limits: Limits) -> Self {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            inner,
            started: Instant::now(),
            received: 0,
            limits,
            tick,
        }
    }

    fn too_slow(&self) -> bool {
        let late = self
            .started
            .elapsed()
            .saturating_sub(self.limits.body_grace);
        self.received < (late.as_secs_f64() * self.limits.body_min_rate as f64) as u64
    }
}

#[derive(Debug)]
struct TooSlow;

impl std::fmt::Display for TooSlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the body arrived too slowly")
    }
}

impl std::error::Error for TooSlow {}

impl http_body::Body for MinRate {
    type Data = axum::body::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.received += data.len() as u64;
                }
                if self.too_slow() {
                    return Poll::Ready(Some(Err(Box::new(TooSlow))));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(Box::new(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                // Wake on a timer so a stalled body is judged without new data.
                while self.tick.poll_tick(cx).is_ready() {
                    if self.too_slow() {
                        return Poll::Ready(Some(Err(Box::new(TooSlow))));
                    }
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
