//! The SQLite settings the operator relies on, under a process killed in the middle of a write.
//!
//! A child process appends log entries as fast as it can and prints each sequence number after
//! its commit returned. The parent kills it without warning, reopens the database and checks
//! that every printed entry is there and the chain is whole. A kill does not cut the power: it
//! proves the journal recovers and that nothing is acknowledged before it is committed. The
//! `synchronous=FULL`, `fullfsync=ON` settings, which make a commit survive a power cut, are
//! checked by name.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use nymira_operator::replog::{kinds, Log};
use nymira_operator::store::Store;

const CHILD_ENV: &str = "NYMIRA_POWER_LOSS_DB";

#[test]
fn the_settings_are_full_synchronous_and_full_fsync() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.durability().unwrap(), (2, 1));
}

/// On a file (an in-memory database ignores WAL) the settings are the same, and the journal is
/// the write-ahead log. A `synchronous=OFF` would pass the kill test below, since SIGKILL leaves
/// the page cache alone, so the settings themselves are asserted.
#[test]
fn a_database_file_is_in_wal_mode_with_full_synchronous() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("op.db");
    let store = Store::open(&path).unwrap();
    assert_eq!(store.durability().unwrap(), (2, 1));
    let mode: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

/// The child half: runs only when the parent set the database path.
#[test]
fn power_loss_child() {
    let Ok(path) = std::env::var(CHILD_ENV) else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let log = Log::new(Store::open(std::path::Path::new(&path)).unwrap());
        let mut out = std::io::stdout();
        let mut i = 0u64;
        loop {
            let entry = log
                .append(
                    kinds::RESERVATION,
                    &format!("lease{i}"),
                    7_200,
                    Some("{}"),
                    false,
                )
                .await
                .unwrap();
            writeln!(out, "{}", entry.seq).unwrap();
            out.flush().unwrap();
            i += 1;
        }
    });
}

#[tokio::test]
async fn a_killed_writer_leaves_a_whole_chain_with_every_acknowledged_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("operator.db");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "power_loss_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, &db)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut last = 0u64;
    let reader = BufReader::new(child.stdout.take().unwrap());
    for line in reader.lines() {
        // The harness prints its own lines too; only bare numbers are acknowledgements.
        if let Ok(seq) = line.unwrap().trim().parse::<u64>() {
            last = seq;
            if last >= 200 {
                break;
            }
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(last >= 200, "the writer produced {last} entries");

    let log = Log::new(Store::open(&db).unwrap());
    let (head, _) = log.head().await.unwrap();
    assert!(
        head >= last,
        "head {head} is behind the last acknowledged entry {last}"
    );
    log.verify().await.unwrap();
}
