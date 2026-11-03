//! A managed Tor process behind a loopback SOCKS5 port.
//!
//! The package is one executable. Its SHA-256 is compared with the pinned hash on every start,
//! and the bytes that were hashed are the bytes that run: they are written to a private copy,
//! so a swap of the original after the check changes nothing. The default backend is C `tor`;
//! `arti proxy` sits behind the same contract (a SOCKS5 listener on 127.0.0.1).
//!
//! Nothing here sends traffic. Callers ask [`Tor::state`] before they build a client, and a
//! state other than `Ready` means no request goes out.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The C implementation, `tor`.
    CTor,
    /// `arti proxy`.
    Arti,
}

impl Backend {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "tor" => Some(Self::CTor),
            "arti" => Some(Self::Arti),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::CTor => "tor",
            Self::Arti => "arti",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TorState {
    /// Started, not yet usable; the percentage of the bootstrap when the process prints one.
    Starting(u8),
    Ready,
    /// The process exited or never became ready. No request goes out.
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum TorError {
    #[error("this build pins no {0} package, so Tor cannot start")]
    NotPinned(&'static str),
    #[error(
        "the {backend} package does not match the pinned hash (pinned {pinned}, found {found})"
    )]
    PackageHash {
        backend: &'static str,
        pinned: String,
        found: String,
    },
    #[error("tor package {path}: {message}")]
    Package { path: PathBuf, message: String },
    #[error("could not start Tor: {0}")]
    Spawn(String),
    #[error("Tor is not ready: {0}")]
    NotReady(String),
}

#[derive(Clone, Debug)]
pub struct TorConfig {
    pub backend: Backend,
    /// The package executable.
    pub package: PathBuf,
    /// The pinned SHA-256 of the package, hex. Empty means this build pins none.
    pub sha256: String,
    /// Private directory for the verified copy and Tor's own state.
    pub work_dir: PathBuf,
}

/// A running Tor. Dropping it stops the process.
pub struct Tor {
    backend: Backend,
    socks: SocketAddr,
    version: String,
    state: watch::Receiver<TorState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tor {
    fn drop(&mut self) {
        // The task owns the child, which is killed when the task is dropped.
        self.task.abort();
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Hashes the package and writes the hashed bytes to a private executable copy.
fn verified_copy(cfg: &TorConfig) -> Result<PathBuf, TorError> {
    let name = cfg.backend.name();
    if cfg.sha256.trim().is_empty() {
        return Err(TorError::NotPinned(name));
    }
    let package_error = |message: String| TorError::Package {
        path: cfg.package.clone(),
        message,
    };
    let bytes = std::fs::read(&cfg.package).map_err(|e| package_error(e.to_string()))?;
    let found = hex::encode(Sha256::digest(&bytes));
    // The pin file holds one hash per platform build, separated by whitespace.
    if !cfg
        .sha256
        .split_whitespace()
        .any(|h| h.eq_ignore_ascii_case(&found))
    {
        return Err(TorError::PackageHash {
            backend: name,
            pinned: cfg
                .sha256
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
            found,
        });
    }
    let bin_dir = cfg.work_dir.join("bin");
    private_dir(&cfg.work_dir)
        .and_then(|()| private_dir(&bin_dir))
        .map_err(|e| package_error(e.to_string()))?;
    std::fs::write(cfg.work_dir.join("empty.torrc"), b"")
        .map_err(|e| package_error(e.to_string()))?;
    let copy = bin_dir.join(name);
    let _ = std::fs::remove_file(&copy);
    std::fs::write(&copy, &bytes).map_err(|e| package_error(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o500))
            .map_err(|e| package_error(e.to_string()))?;
    }
    Ok(copy)
}

/// A free loopback port. The process binds it a moment later; if another process wins the
/// race the child exits and the state is `Failed`, never a silent fallback.
fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn arguments(cfg: &TorConfig, port: u16) -> Vec<String> {
    let data = cfg.work_dir.join("data");
    let empty = cfg.work_dir.join("empty.torrc").display().to_string();
    match cfg.backend {
        // Both config files are ours and empty: a torrc in /etc/tor or the home directory is
        // never read, so nothing on the machine changes the ports or the exit policy.
        Backend::CTor => vec![
            "-f".into(),
            empty.clone(),
            "--defaults-torrc".into(),
            empty,
            "--ClientOnly".into(),
            "1".into(),
            "--SocksPort".into(),
            format!("127.0.0.1:{port} IsolateSOCKSAuth KeepAliveIsolateSOCKSAuth"),
            "--DataDirectory".into(),
            data.display().to_string(),
            "--Log".into(),
            "notice stdout".into(),
        ],
        Backend::Arti => vec![
            "proxy".into(),
            "-o".into(),
            format!("proxy.socks_listen=\"127.0.0.1:{port}\""),
            "-o".into(),
            format!("storage.cache_dir=\"{}\"", data.join("cache").display()),
            "-o".into(),
            format!("storage.state_dir=\"{}\"", data.join("state").display()),
        ],
    }
}

/// The package hash compiled into this build; empty until a package is pinned.
pub fn pinned_sha256(backend: Backend) -> &'static str {
    match backend {
        Backend::CTor => include_str!("../pins/tor.sha256").trim(),
        Backend::Arti => include_str!("../pins/arti.sha256").trim(),
    }
}

/// What one output line of the process says about readiness.
pub fn parse_line(backend: Backend, line: &str) -> Option<TorState> {
    match backend {
        Backend::CTor => {
            let rest = line.split("Bootstrapped ").nth(1)?;
            let percent: u8 = rest.split('%').next()?.trim().parse().ok()?;
            Some(if percent >= 100 {
                TorState::Ready
            } else {
                TorState::Starting(percent)
            })
        }
        Backend::Arti => line
            .contains("Sufficiently bootstrapped")
            .then_some(TorState::Ready),
    }
}

async fn read_version(copy: &Path, backend: Backend) -> Result<String, TorError> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(copy)
            .arg("--version")
            .env_clear()
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| TorError::Spawn("`--version` timed out".into()))?
    .map_err(|e| TorError::Spawn(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .next()
        .map(|l| l.trim().trim_end_matches('.').to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| format!("{} (unknown version)", backend.name())))
}

async fn watch_child(mut child: Child, backend: Backend, state: watch::Sender<TorState>) {
    let mut out = child.stdout.take().map(|o| BufReader::new(o).lines());
    let mut last = String::new();
    loop {
        let next = async {
            match out.as_mut() {
                Some(reader) => reader.next_line().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = next => match line {
                Ok(Some(line)) => {
                    if let Some(next) = parse_line(backend, &line) {
                        let _ = state.send(next);
                    }
                    if line.contains("[err]") || line.contains("[warn]") {
                        last = line;
                    }
                }
                // Output closed: the exit follows.
                _ => out = None,
            },
            status = child.wait() => {
                // The process is gone, but its last lines may still sit in the pipe: they say why.
                if let Some(reader) = out.as_mut() {
                    let drain = async {
                        while let Ok(Some(line)) = reader.next_line().await {
                            if line.contains("[err]") || line.contains("[warn]") {
                                last = line;
                            }
                        }
                    };
                    let _ = tokio::time::timeout(Duration::from_millis(300), drain).await;
                }
                let why = match status {
                    Ok(s) => format!("{} exited ({s})", backend.name()),
                    Err(e) => format!("{} could not be waited on: {e}", backend.name()),
                };
                let why = if last.is_empty() { why } else { format!("{why}: {last}") };
                let _ = state.send(TorState::Failed(why));
                return;
            }
        }
    }
}

impl Tor {
    /// Verifies the package and starts it. Returns at once; the state is `Starting` until the
    /// process reports a finished bootstrap.
    pub async fn start(cfg: &TorConfig) -> Result<Self, TorError> {
        let copy = verified_copy(cfg)?;
        let version = read_version(&copy, cfg.backend).await?;
        let port = free_port().map_err(|e| TorError::Spawn(e.to_string()))?;
        let mut command = Command::new(&copy);
        command
            .args(arguments(cfg, port))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| TorError::Spawn(e.to_string()))?;
        let (tx, rx) = watch::channel(TorState::Starting(0));
        let task = tokio::spawn(watch_child(child, cfg.backend, tx));
        Ok(Self {
            backend: cfg.backend,
            socks: SocketAddr::from(([127, 0, 0, 1], port)),
            version,
            state: rx,
            task,
        })
    }

    /// A handle whose state the test drives, in front of a SOCKS proxy the test runs.
    #[doc(hidden)]
    pub fn for_tests(socks: SocketAddr, state: TorState) -> (Self, watch::Sender<TorState>) {
        let (tx, rx) = watch::channel(state);
        let tor = Self {
            backend: Backend::CTor,
            socks,
            version: "Tor version test".into(),
            state: rx,
            task: tokio::spawn(async {}),
        };
        (tor, tx)
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The loopback SOCKS5 address.
    pub fn socks_addr(&self) -> SocketAddr {
        self.socks
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// `Ok` only while the process is `Ready`; checked before every send on a held client.
    pub fn ensure_ready(&self) -> Result<(), String> {
        match self.state() {
            TorState::Ready => Ok(()),
            TorState::Starting(p) => Err(format!("still starting ({p}%)")),
            TorState::Failed(why) => Err(why),
        }
    }

    pub fn state(&self) -> TorState {
        self.state.borrow().clone()
    }

    /// Waits until the state is `Ready` or `Failed`, or `limit` passes.
    pub async fn wait_ready(&self, limit: Duration) -> Result<(), TorError> {
        let mut rx = self.state.clone();
        let waited = tokio::time::timeout(limit, async {
            loop {
                match rx.borrow_and_update().clone() {
                    TorState::Ready => return Ok(()),
                    TorState::Failed(why) => return Err(TorError::NotReady(why)),
                    TorState::Starting(_) => {}
                }
                if rx.changed().await.is_err() {
                    return Err(TorError::NotReady("the watcher stopped".into()));
                }
            }
        })
        .await;
        waited.unwrap_or_else(|_| {
            Err(TorError::NotReady(format!(
                "not ready after {} s",
                limit.as_secs()
            )))
        })
    }

    /// One line for `nymirad status`.
    pub fn status_line(&self) -> String {
        let state = match self.state() {
            TorState::Ready => "ready".to_string(),
            TorState::Starting(p) => format!("starting ({p}%)"),
            TorState::Failed(why) => format!("DOWN, no request goes out ({why})"),
        };
        format!("Tor: {} {}, {state}", self.backend.name(), self.version)
    }
}

impl std::fmt::Debug for Tor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.status_line())
    }
}

/// A shared handle, since the gateway, the RPC client and the status command all read it.
pub type SharedTor = Arc<Tor>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bootstrap_lines() {
        let c = Backend::CTor;
        assert_eq!(
            parse_line(
                c,
                "Nov 02 12:00:00.000 [notice] Bootstrapped 45% (loading_descriptors): x"
            ),
            Some(TorState::Starting(45))
        );
        assert_eq!(
            parse_line(
                c,
                "Nov 02 12:00:09.000 [notice] Bootstrapped 100% (done): Done"
            ),
            Some(TorState::Ready)
        );
        assert_eq!(parse_line(c, "Opening Socks listener on 127.0.0.1:1"), None);
        assert_eq!(
            parse_line(
                Backend::Arti,
                "INFO arti_client: Sufficiently bootstrapped; system SOCKS proxy now functional."
            ),
            Some(TorState::Ready)
        );
        assert_eq!(parse_line(Backend::Arti, "Bootstrapped 100%"), None);
    }

    #[test]
    fn backend_names_round_trip() {
        for b in [Backend::CTor, Backend::Arti] {
            assert_eq!(Backend::parse(b.name()), Some(b));
        }
        assert_eq!(Backend::parse("nym"), None);
    }
}
