//! The agent Unix socket: a listener of its own that serves agent routes only.
//!
//! The directory is 0750 and the socket 0660, both in the agent group, so other local users
//! cannot open it. The peer's user ID is checked on every connection, in the listener: the
//! owner's own OS user and any user no `unix` agent is registered for are dropped before a
//! byte is read. The check is the only OS-user check there is; loopback TCP has none.

use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::net::{UnixListener, UnixStream};

use crate::agents::{chown_group, Registry, Transport};

pub const SOCKET: &str = "agent.sock";

/// `/tmp/nymira-agent-<uid>`: outside the 0700 state directory, which the agent cannot enter.
pub fn default_dir() -> PathBuf {
    // SAFETY: geteuid has no preconditions.
    PathBuf::from(format!("/tmp/nymira-agent-{}", unsafe { libc::geteuid() }))
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("agent socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Group(String),
}

/// The peer of an accepted connection.
#[derive(Clone, Copy, Debug)]
pub struct PeerUid(pub u32);

/// Where the peer's user ID comes from. Real runs read the socket's credentials; a test that
/// cannot create a second OS user injects its own.
pub type UidSource = Arc<dyn Fn(&UnixStream) -> Option<u32> + Send + Sync>;

pub fn socket_uid() -> UidSource {
    Arc::new(|s| s.peer_cred().ok().map(|c| c.uid()))
}

/// Binds `<dir>/agent.sock`. With a `gid`, the directory and socket are in that group; when the
/// owner is not in it, the directory must already be (the error says which `sudo` command).
pub fn bind(dir: &Path, gid: Option<u32>) -> Result<UnixListener, BindError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o750)
        .create(dir)?;
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let meta = std::fs::metadata(dir)?;
    if meta.uid() != me {
        return Err(BindError::Group(format!(
            "{} is not yours; remove it or pick another agent socket directory",
            dir.display()
        )));
    }
    // Keep the setgid bit an earlier `sudo chmod 2750` set: on Linux it makes the socket inherit
    // the directory's group when the owner cannot chown.
    let setgid = meta.permissions().mode() & 0o2000;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750 | setgid))?;
    if let Some(g) = gid {
        if !chown_group(dir, g) && std::fs::metadata(dir)?.gid() != g {
            let (name, d) = (group_label(g), dir.display());
            return Err(BindError::Group(format!(
                "you are not in the agent group; run: sudo chgrp {name} {d} && sudo chmod 2750 {d}"
            )));
        }
    }
    let path = dir.join(SOCKET);
    if let Ok(old) = std::fs::symlink_metadata(&path) {
        if old.file_type().is_socket() {
            std::fs::remove_file(&path)?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} exists and is not a socket",
                path.display()
            ))
            .into());
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    if let Some(g) = gid {
        let _ = chown_group(&path, g);
        if std::fs::metadata(&path)?.gid() != g {
            return Err(BindError::Group(format!(
                "the socket {} is not in the agent group; run: sudo chgrp {} {}",
                path.display(),
                group_label(g),
                path.display()
            )));
        }
    }
    Ok(listener)
}

fn group_label(gid: u32) -> String {
    crate::agents::group_name(gid).unwrap_or_else(|| gid.to_string())
}

/// Accepts only connections from a user a `unix` agent is registered for, never the owner.
pub struct AgentListener {
    inner: UnixListener,
    registry: Registry,
    owner_uid: u32,
    uid_of: UidSource,
}

impl AgentListener {
    pub fn new(inner: UnixListener, registry: Registry, uid_of: UidSource) -> Self {
        // SAFETY: geteuid has no preconditions.
        let owner_uid = unsafe { libc::geteuid() };
        Self::with_owner(inner, registry, owner_uid, uid_of)
    }

    pub fn with_owner(
        inner: UnixListener,
        registry: Registry,
        owner_uid: u32,
        uid_of: UidSource,
    ) -> Self {
        Self {
            inner,
            registry,
            owner_uid,
            uid_of,
        }
    }

    /// Whether this peer may talk at all.
    fn admits(&self, uid: u32) -> bool {
        uid != self.owner_uid
            && self.registry.load().is_ok_and(|book| {
                book.agents
                    .iter()
                    .any(|a| a.transport == Transport::Unix && a.uid == Some(uid))
            })
    }
}

impl Listener for AgentListener {
    type Io = UnixStream;
    type Addr = PeerUid;

    async fn accept(&mut self) -> (UnixStream, PeerUid) {
        loop {
            let Ok((stream, _)) = self.inner.accept().await else {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            };
            match (self.uid_of)(&stream) {
                Some(uid) if self.admits(uid) => return (stream, PeerUid(uid)),
                // Dropped: the peer sees a closed connection and nothing else.
                _ => drop(stream),
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<PeerUid> {
        Ok(PeerUid(self.owner_uid))
    }
}

impl Connected<IncomingStream<'_, AgentListener>> for PeerUid {
    fn connect_info(stream: IncomingStream<'_, AgentListener>) -> Self {
        *stream.remote_addr()
    }
}

/// Serves the agent routes on `listener` until the process ends.
pub async fn serve(listener: AgentListener, app: axum::Router) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<PeerUid>(),
    )
    .await
}
