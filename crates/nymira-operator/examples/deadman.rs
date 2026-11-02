//! Runs the dead-man switch against an operator's `/v1/status`, with the alarm channels from
//! `~/.nymira-operator.env` (Telegram and email, the same settings as `live_check`). Run it on a
//! machine other than the operator's, so it still speaks when the operator's machine is down.
//!
//! `cargo run -p nymira-operator --example deadman -- https://operator.example/v1/status`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::{
    AlarmSink, Alarms, EmailSink, SmtpConfig, SmtpSecurity, TelegramSink,
};
use nymira_operator::clock::SystemClock;
use nymira_operator::deadman::Deadman;

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
    let Some(status_url) = std::env::args().nth(1) else {
        return eprintln!("usage: deadman <status url>");
    };
    let s = settings();
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let mut sinks: Vec<Arc<dyn AlarmSink>> = Vec::new();
    if !get("TELEGRAM_BOT_TOKEN").is_empty() {
        sinks.push(Arc::new(TelegramSink::new(
            "https://api.telegram.org",
            &get("TELEGRAM_BOT_TOKEN"),
            &get("TELEGRAM_CHAT_ID"),
        )));
    }
    if !get("SMTP_HOST").is_empty() {
        let port = get("SMTP_PORT").parse().unwrap_or(587);
        match EmailSink::new(&SmtpConfig {
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
        }) {
            Ok(sink) => sinks.push(Arc::new(sink)),
            Err(e) => eprintln!("email not built: {e}"),
        }
    }
    if sinks.is_empty() {
        return eprintln!("no alarm channel is configured in ~/.nymira-operator.env");
    }
    let clock = Arc::new(SystemClock);
    let deadman = Arc::new(Deadman::new(
        status_url,
        clock.clone(),
        Arc::new(Alarms::new(sinks, clock)),
    ));
    println!("dead-man switch running");
    deadman.spawn();
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
