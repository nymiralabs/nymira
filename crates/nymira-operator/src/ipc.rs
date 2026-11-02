//! Line-based calls over a Unix socket: one JSON line in, one JSON line out.
//!
//! The socket file is 0600 and the server drops a peer whose user id is not the allowed one
//! before it reads a byte. There is no TCP entry: the server takes a `UnixListener`.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// The most a request or a reply line may hold.
pub const MAX_LINE: u64 = 512 * 1024;
/// A peer that sends nothing for this long is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Binds a socket readable and writable by its owner only. The mode is set on the directory
/// first, so the file is never reachable with a wider mode.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Reads one line of at most [`MAX_LINE`] bytes.
async fn read_line(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<String> {
    let mut line = String::new();
    let read = BufReader::new(stream.take(MAX_LINE))
        .read_line(&mut line)
        .await?;
    if read == 0 || !line.ends_with('\n') {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(line)
}

/// Serves `handler` for as long as the listener lives. A peer whose uid differs from
/// `allowed_uid` is dropped unread; a request that is not a `Req` gets `on_malformed()`.
pub async fn serve<Req, Rep, F, Fut>(
    listener: UnixListener,
    allowed_uid: u32,
    handler: F,
    on_malformed: fn(String) -> Rep,
) where
    Req: DeserializeOwned + Send + 'static,
    Rep: Serialize + Send + 'static,
    F: Fn(Req) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Rep> + Send + 'static,
{
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(allowed_uid) {
            continue;
        }
        let handler = handler.clone();
        tokio::spawn(async move {
            let Ok(Ok(line)) = tokio::time::timeout(READ_TIMEOUT, read_line(&mut stream)).await
            else {
                return;
            };
            let reply = match serde_json::from_str::<Req>(&line) {
                Ok(request) => handler(request).await,
                Err(e) => on_malformed(e.to_string()),
            };
            let Ok(mut bytes) = serde_json::to_vec(&reply) else {
                return;
            };
            bytes.push(b'\n');
            let _ = stream.write_all(&bytes).await;
        });
    }
}

/// Sends one request and reads one reply.
pub async fn call<Req: Serialize, Rep: DeserializeOwned>(
    path: &Path,
    request: &Req,
    wait: Duration,
) -> std::io::Result<Rep> {
    let exchange = async {
        let mut stream = UnixStream::connect(path).await?;
        let mut bytes = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let line = read_line(&mut stream).await?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    };
    tokio::time::timeout(wait, exchange)
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
}
