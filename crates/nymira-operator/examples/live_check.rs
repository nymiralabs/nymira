//! Sends one test alarm to Telegram and to the project mailbox, from the settings in
//! `~/.nymira-operator.env`. Prints only whether each channel took it, never a setting.
//!
//! `cargo run -p nymira-operator --example live_check`

use std::collections::HashMap;

use nymira_operator::alarm::{Alarm, AlarmSink, EmailSink, SmtpConfig, SmtpSecurity, TelegramSink};

fn settings() -> HashMap<String, String> {
    let path = std::env::var("HOME").map(|h| format!("{h}/.nymira-operator.env"));
    let text = path
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

#[tokio::main]
async fn main() {
    let s = settings();
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let alarm = Alarm {
        key: "live_check".into(),
        subject: "Nymira operator test alarm".into(),
        body: "If you can read this, this channel works.".into(),
    };

    let telegram = TelegramSink::new(
        "https://api.telegram.org",
        &get("TELEGRAM_BOT_TOKEN"),
        &get("TELEGRAM_CHAT_ID"),
    );
    match telegram.send(&alarm).await {
        Ok(()) => println!("telegram: sent"),
        Err(e) => println!("telegram: failed ({e})"),
    }

    let port = get("SMTP_PORT").parse().unwrap_or(587);
    let email = EmailSink::new(&SmtpConfig {
        host: get("SMTP_HOST"),
        port,
        security: if port == 465 {
            SmtpSecurity::Tls
        } else {
            SmtpSecurity::StartTls
        },
        username: Some(get("SMTP_USER")),
        password: Some(get("SMTP_PASSWORD")),
        from: get("SMTP_FROM"),
        to: get("ALERT_EMAIL_TO"),
    });
    match email {
        Ok(sink) => match sink.send(&alarm).await {
            Ok(()) => println!("email: sent"),
            Err(e) => println!("email: failed ({e})"),
        },
        Err(e) => println!("email: not built ({e})"),
    }
}
