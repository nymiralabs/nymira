//! The file `serve` leaves for `status`: written once the port is bound, removed on exit, and
//! never read as "relay" when it is stale or unreadable.

use std::sync::Arc;

use nymirad::nearai::{clear_mode, read_mode, save_mode};
use nymirad::token::OwnerToken;

#[test]
fn the_mode_file_says_the_mode_of_a_live_serve_and_fails_when_stale_or_unreadable() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // No file: no serve is running.
    assert_eq!(read_mode(h), Ok(None));
    save_mode(h, true).unwrap();
    assert_eq!(read_mode(h), Ok(Some(true)));
    save_mode(h, false).unwrap();
    assert_eq!(read_mode(h), Ok(Some(false)));

    // A process that is gone left it behind.
    std::fs::write(h.join("nras-mode"), "direct 4194300\n").unwrap();
    let e = read_mode(h).unwrap_err();
    assert!(e.contains("stale"), "{e}");
    // Garbage, an old one-word file, a wrong word.
    for text in [
        "",
        "relay\n",
        "direct\n",
        "sideways 1\n",
        "relay x\n",
        "\0\0",
    ] {
        std::fs::write(h.join("nras-mode"), text).unwrap();
        assert!(read_mode(h).is_err(), "{text:?}");
    }
    // A directory where the file should be.
    std::fs::remove_file(h.join("nras-mode")).unwrap();
    std::fs::create_dir(h.join("nras-mode")).unwrap();
    assert!(read_mode(h).is_err());
}

#[test]
fn a_save_that_fails_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    assert!(save_mode(&home.path().join("missing/dir"), false).is_err());
}

#[test]
fn clearing_removes_only_the_file_of_this_process() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    save_mode(h, true).unwrap();
    clear_mode(h);
    assert_eq!(read_mode(h), Ok(None));
    // Process 1 is alive and not us: another serve's file stays.
    std::fs::write(h.join("nras-mode"), "direct 1\n").unwrap();
    clear_mode(h);
    assert!(h.join("nras-mode").exists());
}

/// A second `serve` that cannot bind the port leaves the first one's file alone, and the file is
/// written only after the bind.
#[tokio::test]
async fn the_mode_is_written_after_the_bind_and_not_when_it_fails() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let token = || {
        let dir = tempfile::tempdir().unwrap();
        let t = OwnerToken::load_or_create(&dir.path().join("owner.token")).unwrap();
        (dir, Arc::new(t))
    };

    let taken = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let busy = taken.local_addr().unwrap().port();
    let (_d, t) = token();
    let h2 = h.clone();
    let result = nymirad::guard::serve(t, None, busy, move || save_mode(&h2, false)).await;
    assert!(result.is_err(), "the port was taken");
    assert!(
        !h.join("nras-mode").exists(),
        "nothing written before the bind"
    );
    drop(taken);

    let (_d, t) = token();
    let h2 = h.clone();
    let server = tokio::spawn(async move {
        let _ = nymirad::guard::serve(t, None, 0, move || save_mode(&h2, true)).await;
    });
    for _ in 0..100 {
        if h.join("nras-mode").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(read_mode(&h), Ok(Some(true)));
    server.abort();
}

/// If the callback fails, `serve` fails: a mode nobody can read is not served.
#[tokio::test]
async fn a_failing_save_stops_serve() {
    let dir = tempfile::tempdir().unwrap();
    let t = Arc::new(OwnerToken::load_or_create(&dir.path().join("owner.token")).unwrap());
    let result = nymirad::guard::serve(t, None, 0, || {
        Err(std::io::Error::other("cannot write the mode file"))
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("mode file"));
}
