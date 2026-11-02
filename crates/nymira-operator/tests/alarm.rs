//! Alarm delivery: the repeat window, Telegram and email against local stubs.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use nymira_operator::alarm::{
    Alarm, AlarmSink, Alarms, EmailSink, FakeSink, SmtpConfig, SmtpSecurity, TelegramSink,
};
use nymira_operator::clock::ManualClock;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn alarm() -> Alarm {
    Alarm {
        key: "credit_low".into(),
        subject: "NEAR AI credit is low".into(),
        body: "0.40 USD left".into(),
    }
}

#[tokio::test]
async fn an_alarm_repeats_after_the_window_or_after_the_condition_clears() {
    let clock = Arc::new(ManualClock::new(1_000_000 * 1000));
    let sink = Arc::new(FakeSink::default());
    let alarms = Alarms::new(vec![sink.clone()], clock.clone()).with_repeat_secs(3600);
    assert!(alarms.raise("k", "s", "b").await);
    assert!(!alarms.raise("k", "s", "b").await, "inside the window");
    assert!(alarms.active("k"));
    assert!(
        alarms.raise("other", "s", "b").await,
        "another key is independent"
    );
    clock.advance_secs(3600);
    assert!(alarms.raise("k", "s", "b").await);
    alarms.resolve("k");
    assert!(
        alarms.raise("k", "s", "b").await,
        "a cleared condition raises at once"
    );
    assert_eq!(sink.sent().len(), 4);
}

#[tokio::test]
async fn an_alarm_no_sink_took_is_tried_again_and_one_good_sink_is_enough() {
    let clock = Arc::new(ManualClock::new(1_000_000 * 1000));
    let (good, bad) = (Arc::new(FakeSink::default()), Arc::new(FakeSink::default()));
    bad.set_fail(true);
    let alarms = Alarms::new(vec![bad.clone(), good.clone()], clock.clone());
    assert!(alarms.raise("k", "s", "b").await);
    assert_eq!((good.sent().len(), bad.sent().len()), (1, 0));

    good.set_fail(true);
    assert!(!alarms.raise("fresh", "s", "b").await);
    assert!(!alarms.active("fresh"));
    good.set_fail(false);
    assert!(
        alarms.raise("fresh", "s", "b").await,
        "not marked sent, so it is retried"
    );
}

#[tokio::test]
async fn telegram_gets_the_text_in_the_bot_url_and_a_failure_does_not_leak_the_token() {
    type Seen = Arc<Mutex<Vec<(String, Value)>>>;
    async fn handle(
        State(seen): State<Seen>,
        Path(bot): Path<String>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        seen.lock().unwrap().push((bot, body));
        Json(json!({"ok": true}))
    }
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route("/{bot}/sendMessage", post(handle))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let sink = TelegramSink::new(&base, "12345:SECRET", "-100777");
    sink.send(&alarm()).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "bot12345:SECRET");
        assert_eq!(seen[0].1["chat_id"], "-100777");
        assert_eq!(seen[0].1["text"], "NEAR AI credit is low\n\n0.40 USD left");
    }

    let dead = TelegramSink::new("http://127.0.0.1:1", "12345:SECRET", "-100777");
    let error = dead.send(&alarm()).await.unwrap_err();
    assert!(!error.contains("SECRET"), "{error}");
}

/// A one-connection SMTP server that records the message it is given.
async fn smtp_stub() -> (u16, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        write.write_all(b"220 stub ESMTP\r\n").await.unwrap();
        let mut message = String::new();
        let mut data = false;
        let mut line = String::new();
        loop {
            line.clear();
            if read.read_line(&mut line).await.unwrap() == 0 {
                return message;
            }
            if data {
                if line == ".\r\n" {
                    data = false;
                    write.write_all(b"250 queued\r\n").await.unwrap();
                } else {
                    message.push_str(&line);
                }
                continue;
            }
            let upper = line.to_ascii_uppercase();
            let reply: &[u8] = if upper.starts_with("EHLO") {
                b"250 stub\r\n"
            } else if upper.starts_with("DATA") {
                data = true;
                b"354 go\r\n"
            } else if upper.starts_with("QUIT") {
                write.write_all(b"221 bye\r\n").await.unwrap();
                return message;
            } else {
                b"250 ok\r\n"
            };
            write.write_all(reply).await.unwrap();
        }
    });
    (port, task)
}

#[tokio::test]
async fn email_is_sent_over_smtp_to_the_project_address() {
    let (port, stub) = smtp_stub().await;
    let sink = EmailSink::new(&SmtpConfig {
        host: "127.0.0.1".into(),
        port,
        security: SmtpSecurity::Plain,
        username: None,
        password: None,
        from: "Nymira operator <operator@example.test>".into(),
        to: "alerts@example.test".into(),
    })
    .unwrap();
    sink.send(&alarm()).await.unwrap();
    let message = stub.await.unwrap();
    assert!(
        message.contains("Subject: NEAR AI credit is low"),
        "{message}"
    );
    assert!(message.contains("To: alerts@example.test"), "{message}");
    assert!(message.contains("0.40 USD left"), "{message}");
}

#[tokio::test]
async fn bad_addresses_are_refused_when_the_sink_is_built() {
    let cfg = |from: &str, to: &str| SmtpConfig {
        host: "127.0.0.1".into(),
        port: 25,
        security: SmtpSecurity::Plain,
        username: None,
        password: None,
        from: from.into(),
        to: to.into(),
    };
    assert!(EmailSink::new(&cfg("not an address", "a@example.test")).is_err());
    assert!(EmailSink::new(&cfg("a@example.test", "")).is_err());
}
