//! Alarm delivery to the project email and the project Telegram bot.
//!
//! The credit watcher raises alarms here; the chain watchers (#27, #88) reuse the same hub. An
//! alarm has a key: while the same key is raised again it is sent at most once per repeat
//! window, and [`Alarms::resolve`] makes the next raise go out at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::clock::Clock;

/// An alarm repeats at most this often while its condition holds.
pub const REPEAT_SECS: u64 = 6 * 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alarm {
    /// Stable name of the condition, such as `credit_low`.
    pub key: String,
    pub subject: String,
    pub body: String,
}

#[async_trait]
pub trait AlarmSink: Send + Sync {
    /// The error text never holds a token or a password.
    async fn send(&self, alarm: &Alarm) -> Result<(), String>;
}

pub struct Alarms {
    sinks: Vec<Arc<dyn AlarmSink>>,
    clock: Arc<dyn Clock>,
    repeat_secs: u64,
    sent: Mutex<HashMap<String, u64>>,
}

impl Alarms {
    pub fn new(sinks: Vec<Arc<dyn AlarmSink>>, clock: Arc<dyn Clock>) -> Self {
        Self {
            sinks,
            clock,
            repeat_secs: REPEAT_SECS,
            sent: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_repeat_secs(mut self, secs: u64) -> Self {
        self.repeat_secs = secs;
        self
    }

    /// Sends the alarm to every sink unless the same key went out within the repeat window.
    /// Returns true when at least one sink took it. A key whose every sink failed is not marked
    /// sent, so the next tick tries again.
    pub async fn raise(&self, key: &str, subject: &str, body: &str) -> bool {
        let now = self.clock.now_secs();
        {
            let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            if sent
                .get(key)
                .is_some_and(|at| now < at.saturating_add(self.repeat_secs))
            {
                return false;
            }
        }
        let alarm = Alarm {
            key: key.into(),
            subject: crate::redact::scrub(subject),
            body: crate::redact::scrub(body),
        };
        let mut delivered = false;
        for sink in &self.sinks {
            delivered |= sink.send(&alarm).await.is_ok();
        }
        if delivered {
            self.sent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key.into(), now);
        }
        delivered
    }

    /// The condition is over: the next raise of `key` is sent at once.
    pub fn resolve(&self, key: &str) {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    /// Whether the key went out and is still inside its repeat window.
    pub fn active(&self, key: &str) -> bool {
        let now = self.clock.now_secs();
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .is_some_and(|at| now < at.saturating_add(self.repeat_secs))
    }
}

/// Records alarms; `fail` makes every send fail.
#[derive(Default)]
pub struct FakeSink {
    sent: Mutex<Vec<Alarm>>,
    fail: Mutex<bool>,
}

impl FakeSink {
    pub fn sent(&self) -> Vec<Alarm> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap_or_else(|e| e.into_inner()) = fail;
    }
}

#[async_trait]
impl AlarmSink for FakeSink {
    async fn send(&self, alarm: &Alarm) -> Result<(), String> {
        if *self.fail.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err("fake sink failure".into());
        }
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(alarm.clone());
        Ok(())
    }
}

/// Telegram Bot API `sendMessage`. The bot token sits in the URL, so errors are sent through
/// `without_url`.
pub struct TelegramSink {
    api_base: String,
    token: String,
    chat_id: String,
    http: reqwest::Client,
}

impl TelegramSink {
    /// `api_base` is `https://api.telegram.org` outside tests.
    pub fn new(api_base: &str, token: &str, chat_id: &str) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').into(),
            token: token.into(),
            chat_id: chat_id.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("client builds"),
        }
    }
}

#[async_trait]
impl AlarmSink for TelegramSink {
    async fn send(&self, alarm: &Alarm) -> Result<(), String> {
        let url = format!("{}/bot{}/sendMessage", self.api_base, self.token);
        let reply = self
            .http
            .post(url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": format!("{}\n\n{}", alarm.subject, alarm.body),
            }))
            .send()
            .await
            .map_err(|e| format!("telegram: {}", e.without_url()))?;
        if reply.status().is_success() {
            Ok(())
        } else {
            Err(format!("telegram answered {}", reply.status()))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpSecurity {
    /// Implicit TLS (port 465).
    Tls,
    /// STARTTLS (port 587).
    StartTls,
    /// Plain text: tests against a local stub only.
    Plain,
}

pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub to: String,
}

pub struct EmailSink {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    to: Mailbox,
}

impl EmailSink {
    pub fn new(cfg: &SmtpConfig) -> Result<Self, String> {
        let builder = match cfg.security {
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host),
            SmtpSecurity::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)
            }
            SmtpSecurity::Plain => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &cfg.host,
            )),
        }
        .map_err(|e| format!("smtp host: {e}"))?;
        let mut builder = builder
            .port(cfg.port)
            .timeout(Some(Duration::from_secs(15)));
        if let (Some(user), Some(pass)) = (&cfg.username, &cfg.password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from: cfg
                .from
                .parse()
                .map_err(|_| "smtp from address".to_string())?,
            to: cfg.to.parse().map_err(|_| "smtp to address".to_string())?,
        })
    }
}

#[async_trait]
impl AlarmSink for EmailSink {
    async fn send(&self, alarm: &Alarm) -> Result<(), String> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(self.to.clone())
            .subject(alarm.subject.clone())
            .body(alarm.body.clone())
            .map_err(|e| format!("email: {e}"))?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|e| format!("email: {e}"))
    }
}
