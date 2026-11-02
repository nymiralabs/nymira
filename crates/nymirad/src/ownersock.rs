//! The owner socket: how the CLI reaches the daemon for everything that moves money or touches
//! a key (`close`, `exit`, `finalize`, `claim-payout`, submitters). A Unix socket inside a 0700
//! directory, so reading the owner token file does not hand these powers over; only a process of
//! the same user can connect, and the server checks that with the peer's credentials.
//!
//! One request per connection, as one JSON line. The server answers with lines: `say` (progress),
//! `ask` (a question for the user's terminal; the client answers with `{"answer": bool}`), then
//! `ok` or `err`.

use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::gateway::close::{Ask, CloseOptions, LeaveError};
use crate::gateway::Gateway;

pub const DIR: &str = "run";
pub const SOCKET: &str = "owner.sock";
const LINE_CAP: usize = 4096;

pub fn socket_path(home: &Path) -> PathBuf {
    home.join(DIR).join(SOCKET)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Close {
        note: u32,
        /// Seconds to wait before the ownership proof goes out; random when absent.
        delay_secs: Option<u64>,
        family: Option<String>,
        via_operator: bool,
    },
    Exit {
        note: u32,
        family: Option<String>,
    },
    Finalize {
        note: u32,
        family: Option<String>,
    },
    ClaimPayout {
        note: u32,
        family: Option<String>,
    },
    SubmitterAdd {
        family: String,
        account: String,
    },
    SubmitterAssign {
        note: u32,
        family: String,
    },
    SubmitterList,
    /// Writes the encrypted backup of the whole state now.
    Backup,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    Say(String),
    Ask(String),
    Ok(String),
    Err(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    #[error("owner socket: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "the directory {0} is not private to you (mode {1:o}, owner {2}); fix it or remove it"
    )]
    Dir(PathBuf, u32, u32),
}

/// Binds the socket: the directory is made 0700 (and refused if it is not ours or looser), an old
/// socket file is replaced, the socket itself is 0600.
pub fn bind(home: &Path) -> Result<UnixListener, SocketError> {
    let dir = home.join(DIR);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = std::fs::metadata(&dir)?;
    let (mode, owner) = (meta.permissions().mode() & 0o777, meta.uid());
    // SAFETY: geteuid has no preconditions.
    if mode & 0o077 != 0 || owner != unsafe { libc::geteuid() } {
        return Err(SocketError::Dir(dir, mode, owner));
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
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub struct Wire {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}

/// The question channel back to the CLI, which asks on its terminal.
pub struct SocketAsk(Mutex<Wire>);

async fn send(wire: &mut Wire, line: &Line) {
    let mut text = serde_json::to_string(line).expect("line serializes");
    text.push('\n');
    let _ = wire.writer.write_all(text.as_bytes()).await;
}

#[async_trait]
impl Ask for SocketAsk {
    async fn confirm_direct(&self, text: &str) -> bool {
        let mut wire = self.0.lock().await;
        send(&mut wire, &Line::Ask(text.into())).await;
        let mut answer = String::new();
        // No answer in two minutes is a no.
        let read =
            tokio::time::timeout(Duration::from_secs(120), wire.reader.read_line(&mut answer))
                .await;
        matches!(read, Ok(Ok(n)) if n > 0)
            && serde_json::from_str::<serde_json::Value>(&answer)
                .ok()
                .and_then(|v| v["answer"].as_bool())
                == Some(true)
    }
}

/// Serves until the process ends.
pub async fn serve(listener: UnixListener, gw: Arc<Gateway>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        // SAFETY: geteuid has no preconditions.
        let ours = stream
            .peer_cred()
            .is_ok_and(|c| c.uid() == unsafe { libc::geteuid() });
        if !ours {
            continue;
        }
        let gw = gw.clone();
        tokio::spawn(async move { handle(stream, gw).await });
    }
}

async fn handle(stream: UnixStream, gw: Arc<Gateway>) {
    let (read, writer) = stream.into_split();
    let mut wire = Wire {
        writer,
        reader: BufReader::new(read),
    };
    let mut first = String::new();
    let got = (&mut wire.reader)
        .take(LINE_CAP as u64)
        .read_line(&mut first)
        .await;
    let command = match got {
        Ok(n) if n > 0 => serde_json::from_str::<Command>(&first),
        _ => return,
    };
    let command = match command {
        Ok(c) => c,
        Err(e) => {
            send(&mut wire, &Line::Err(format!("not a command: {e}"))).await;
            return;
        }
    };
    let wire = Mutex::new(wire);
    let ask = SocketAsk(wire);
    let outcome = run(&gw, command, &ask).await;
    let mut wire = ask.0.lock().await;
    let line = match outcome {
        Ok(text) => Line::Ok(text),
        Err(e) => Line::Err(e),
    };
    send(&mut wire, &line).await;
}

async fn say(ask: &SocketAsk, text: String) {
    send(&mut *ask.0.lock().await, &Line::Say(text)).await;
}

/// Runs one command and returns what to tell the user.
pub async fn run(gw: &Gateway, command: Command, ask: &SocketAsk) -> Result<String, String> {
    let err = |e: LeaveError| e.to_string();
    match command {
        Command::Close {
            note,
            delay_secs,
            family,
            via_operator,
        } => {
            let mut opts = CloseOptions::default();
            if let Some(d) = delay_secs {
                opts.delay = Duration::from_secs(d);
            }
            opts.family = family;
            opts.via_operator = via_operator;
            say(
                ask,
                format!(
                    "Closing note {note}. The ownership proof goes out in {} s, on its own Tor circuit.",
                    opts.delay.as_secs()
                ),
            )
            .await;
            let c = gw.close_note(note, opts, Some(ask)).await.map_err(err)?;
            Ok(if c.confirmed {
                format!(
                    "Note {} is closed. The balance goes to {}. Transaction {}.",
                    c.note_id, c.payout_account, c.tx_hash
                )
            } else {
                format!(
                    "The operator took the close of note {} (transaction {}), but the vault does not show it yet. Run the command again to check.",
                    c.note_id, c.tx_hash
                )
            })
        }
        Command::Exit { note, family } => {
            let e = gw
                .exit_note(note, family.as_deref(), Some(ask))
                .await
                .map_err(err)?;
            Ok(format!(
                "Exit filed for note {} (conceded {}). The window ends at Unix time {}; while nymirad runs it sends finalize_exit then, or run `nymirad finalize --note {}`.",
                e.note_id, e.conceded, e.deadline_secs, e.note_id
            ))
        }
        Command::Finalize { note, family } => {
            let p = gw
                .finalize_note(note, family.as_deref(), Some(ask))
                .await
                .map_err(err)?;
            Ok(format!(
                "Exit of note {} finalized. Transaction {}.",
                p.note_id, p.tx_hash
            ))
        }
        Command::ClaimPayout { note, family } => {
            let p = gw
                .claim_payout(note, family.as_deref(), Some(ask))
                .await
                .map_err(err)?;
            Ok(format!(
                "Payout of note {} claimed. Transaction {}.",
                p.note_id, p.tx_hash
            ))
        }
        Command::SubmitterAdd { family, account } => {
            let sub = gw
                .submitters()
                .add(&family, &account)
                .map_err(|e| e.to_string())?;
            Ok(crate::submitter::instructions(&sub, gw.vault_account()))
        }
        Command::SubmitterAssign { note, family } => {
            let commitment = gw.commitment_of(note).map_err(err)?;
            gw.submitters()
                .assign(&commitment, &family)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "Note {note} uses the submitter of family {family:?}."
            ))
        }
        Command::Backup => {
            let written = gw.write_backup().map_err(|e| e.to_string())?;
            Ok(if written.is_empty() {
                "No backup is set up: run `nymirad backup-setup`.".to_string()
            } else {
                format!(
                    "Backup written to {} place(s):\n{}",
                    written.len(),
                    written
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
        }
        Command::SubmitterList => {
            let list = gw.submitters().list().map_err(|e| e.to_string())?;
            Ok(if list.is_empty() {
                "No submitter accounts.".into()
            } else {
                list.iter()
                    .map(|(f, a)| format!("{f}: {a}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
    }
}

/// The client side: sends `command`, hands progress to `on_say`, questions to `on_ask`, and
/// returns the final text.
pub async fn request(
    home: &Path,
    command: &Command,
    mut on_say: impl FnMut(&str),
    mut on_ask: impl FnMut(&str) -> bool,
) -> Result<String, String> {
    let path = socket_path(home);
    let stream = UnixStream::connect(&path).await.map_err(|e| {
        format!(
            "cannot reach the owner socket {}: {e} (is `nymirad serve` running?)",
            path.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    let mut text = serde_json::to_string(command).expect("command serializes");
    text.push('\n');
    write
        .write_all(text.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        match serde_json::from_str::<Line>(&line).map_err(|e| e.to_string())? {
            Line::Say(s) => on_say(&s),
            Line::Ask(q) => {
                let yes = on_ask(&q);
                let mut a = serde_json::json!({ "answer": yes }).to_string();
                a.push('\n');
                write
                    .write_all(a.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Line::Ok(t) => return Ok(t),
            Line::Err(e) => return Err(e),
        }
    }
    Err("the daemon closed the connection without an answer".into())
}
