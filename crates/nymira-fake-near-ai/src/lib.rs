//! Fake NEAR AI Cloud service for tests and the local demo stack. See the README.

pub mod crypto;
pub mod policy;
mod routes;
mod state;

pub use policy::{
    verify_attestation, verify_signature, SignatureKind, TestPolicy, VerifiedAttestation,
};
pub use routes::router;
pub use state::{
    ChatRecord, Handle, KeyRecord, SpendLimit, Switches, Usage, UsageRow, FIRST_REFRESH_TOKEN,
    MODELS, REPORTING_TOKEN, SESSION_TOKEN,
};

use std::net::SocketAddr;
use tokio::{net::TcpListener, task::JoinHandle};

/// A fake serving on a bound socket. Dropping it stops the server.
pub struct Running {
    pub addr: SocketAddr,
    pub handle: Handle,
    task: JoinHandle<()>,
}

impl Running {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds `addr` (port 0 picks a free port) and serves in a background task.
pub async fn start(addr: SocketAddr) -> std::io::Result<Running> {
    let listener = TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    let handle = Handle::default();
    let app = router(handle.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Running { addr, handle, task })
}
