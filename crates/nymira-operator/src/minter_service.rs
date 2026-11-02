//! The key minter as its own process: the only holder of the NEAR AI login and reporting
//! tokens, and of the key that signs usage records.
//!
//! The public API process reaches it over a Unix socket ([`RemoteProvider`]). It gets each
//! lease key's secret back when the key is minted (a lease key is capped at the lease's bound),
//! chats with it directly, and never sees a token. Usage records are signed here, after the
//! minter has read the usage itself: the API process cannot ask for a record of a charge the
//! provider did not report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::ipc;
use crate::nearai::{LeaseChat, NearAiAdapter};
use crate::provider::{
    ChatRequest, ChatResponse, EvidenceQuery, KeyUsage, LiveModel, MintedKey, ProviderAdapter,
    ProviderError,
};
use crate::usage::{Minter, SignedUsage, UsageRecord, UsageSigner};

/// A usage record may be this far from the minter's clock.
const READ_SKEW_SECS: u64 = 120;
const WAIT: Duration = Duration::from_secs(60);

/// What the minter needs from the provider side besides the adapter trait: a lease key's
/// secret, handed to the process that proxies its chat.
#[async_trait]
pub trait SecretSource: ProviderAdapter {
    async fn secret(&self, lease_id: &str) -> Option<String>;
    /// True once the key is deactivated: its usage no longer grows.
    async fn is_revoked(&self, lease_id: &str) -> bool;
}

#[async_trait]
impl SecretSource for NearAiAdapter {
    async fn secret(&self, lease_id: &str) -> Option<String> {
        NearAiAdapter::secret(self, lease_id)
    }

    async fn is_revoked(&self, lease_id: &str) -> bool {
        NearAiAdapter::is_revoked(self, lease_id)
    }
}

#[async_trait]
impl SecretSource for crate::provider::FakeProvider {
    async fn secret(&self, lease_id: &str) -> Option<String> {
        self.is_live(lease_id)
            .then(|| format!("fake-secret-{lease_id}"))
    }

    async fn is_revoked(&self, lease_id: &str) -> bool {
        self.keys().get(lease_id).is_some_and(|k| k.revoked)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Call {
    Mint { lease_id: String, bound: String },
    Secret { lease_id: String },
    Usage { lease_id: String },
    SignUsage { record: UsageRecord },
    Revoke { lease_id: String },
    List,
    Balance,
    Models,
    Housekeeping,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum Reply {
    Minted {
        key_id: String,
        expires_at: u64,
        secret: Option<String>,
    },
    Secret(Option<String>),
    Usage {
        cost_nano_usd: String,
    },
    Signed(SignedUsage),
    Leases(Vec<String>),
    Balance(String),
    Models(Option<Vec<LiveModel>>),
    Done,
    Failed(ProviderError),
}

/// Serves the minter to the one allowed user.
pub async fn serve(
    listener: tokio::net::UnixListener,
    provider: Arc<dyn SecretSource>,
    signer: Minter,
    clock: Arc<dyn Clock>,
    uid: u32,
) {
    let handler = move |call: Call| {
        let (provider, signer, clock) = (provider.clone(), signer.clone(), clock.clone());
        async move {
            match answer(&*provider, &signer, &*clock, call).await {
                Ok(reply) => reply,
                Err(e) => Reply::Failed(e),
            }
        }
    };
    ipc::serve(listener, uid, handler, |why| {
        Reply::Failed(ProviderError::Rejected(format!(
            "not a typed request: {why}"
        )))
    })
    .await;
}

async fn answer(
    provider: &dyn SecretSource,
    signer: &Minter,
    clock: &dyn Clock,
    call: Call,
) -> Result<Reply, ProviderError> {
    Ok(match call {
        Call::Mint { lease_id, bound } => {
            let bound: u128 = bound
                .parse()
                .map_err(|_| ProviderError::Rejected("bound is not a number".into()))?;
            let MintedKey { key_id, expires_at } = provider.mint(&lease_id, bound).await?;
            Reply::Minted {
                key_id,
                expires_at,
                secret: provider.secret(&lease_id).await,
            }
        }
        Call::Secret { lease_id } => Reply::Secret(provider.secret(&lease_id).await),
        Call::Usage { lease_id } => Reply::Usage {
            cost_nano_usd: provider.usage(&lease_id).await?.cost_nano_usd.to_string(),
        },
        Call::SignUsage { record } => {
            // A live key is still spending: a reading taken now is not the lease's final one.
            if !provider.is_revoked(&record.lease_id).await {
                return Err(ProviderError::Rejected("the key is still live".into()));
            }
            let now = clock.now_secs();
            if record.read_at.abs_diff(now) > READ_SKEW_SECS {
                return Err(ProviderError::Rejected("the reading time is off".into()));
            }
            // The minter reads the usage again itself: the caller only names the lease.
            let read = provider.usage(&record.lease_id).await.ok();
            let agrees = match (&record.cost_nano_usd, read) {
                (Some(cost), Some(u)) => *cost == u.cost_nano_usd.to_string(),
                (None, None) => true,
                _ => false,
            };
            if !agrees {
                return Err(ProviderError::Rejected(
                    "the record is not what the provider reports".into(),
                ));
            }
            Reply::Signed(signer.sign(record))
        }
        Call::Revoke { lease_id } => {
            provider.revoke(&lease_id).await?;
            Reply::Done
        }
        Call::List => Reply::Leases(provider.list().await?),
        Call::Balance => Reply::Balance(provider.balance().await?.to_string()),
        Call::Models => Reply::Models(provider.models().await?),
        Call::Housekeeping => {
            provider.housekeeping().await;
            Reply::Done
        }
    })
}

/// The minter as the public API process sees it. Lease key secrets are kept in memory only.
pub struct RemoteProvider {
    path: PathBuf,
    chat: LeaseChat,
    secrets: Mutex<HashMap<String, String>>,
}

impl RemoteProvider {
    pub fn new(path: &Path, chat: LeaseChat) -> Self {
        Self {
            path: path.into(),
            chat,
            secrets: Mutex::new(HashMap::new()),
        }
    }

    async fn ask(&self, call: Call) -> Result<Reply, ProviderError> {
        let reply: Reply = ipc::call(&self.path, &call, WAIT)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("the key minter: {e}")))?;
        match reply {
            Reply::Failed(e) => Err(e),
            other => Ok(other),
        }
    }

    fn remember(&self, lease_id: &str, secret: &str) {
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(lease_id.into(), secret.into());
    }

    /// The lease key's secret: from memory, else from the minter (after an API restart).
    async fn secret(&self, lease_id: &str) -> Result<String, ProviderError> {
        if let Some(s) = self
            .secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(lease_id)
        {
            return Ok(s.clone());
        }
        match self
            .ask(Call::Secret {
                lease_id: lease_id.into(),
            })
            .await?
        {
            Reply::Secret(Some(secret)) => {
                self.remember(lease_id, &secret);
                Ok(secret)
            }
            Reply::Secret(None) => Err(ProviderError::NotFound("key is not live".into())),
            _ => Err(other_answer()),
        }
    }
}

fn other_answer() -> ProviderError {
    ProviderError::Unavailable("the key minter sent another answer".into())
}

#[async_trait]
impl ProviderAdapter for RemoteProvider {
    async fn chat(
        &self,
        lease_id: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        let secret = self.secret(lease_id).await?;
        self.chat.chat(&secret, lease_id, request).await
    }

    async fn models(&self) -> Result<Option<Vec<LiveModel>>, ProviderError> {
        match self.ask(Call::Models).await? {
            Reply::Models(m) => Ok(m),
            _ => Err(other_answer()),
        }
    }

    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError> {
        match self
            .ask(Call::Mint {
                lease_id: lease_id.into(),
                bound: bound.to_string(),
            })
            .await?
        {
            Reply::Minted {
                key_id,
                expires_at,
                secret,
            } => {
                if let Some(secret) = secret {
                    self.remember(lease_id, &secret);
                }
                Ok(MintedKey { key_id, expires_at })
            }
            _ => Err(other_answer()),
        }
    }

    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError> {
        match self
            .ask(Call::Usage {
                lease_id: lease_id.into(),
            })
            .await?
        {
            Reply::Usage { cost_nano_usd } => Ok(KeyUsage {
                cost_nano_usd: cost_nano_usd.parse().map_err(|_| other_answer())?,
            }),
            _ => Err(other_answer()),
        }
    }

    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError> {
        self.ask(Call::Revoke {
            lease_id: lease_id.into(),
        })
        .await?;
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(lease_id);
        self.chat.forget(lease_id);
        Ok(())
    }

    async fn housekeeping(&self) {
        let _ = self.ask(Call::Housekeeping).await;
    }

    async fn list(&self) -> Result<Vec<String>, ProviderError> {
        match self.ask(Call::List).await? {
            Reply::Leases(l) => Ok(l),
            _ => Err(other_answer()),
        }
    }

    async fn balance(&self) -> Result<u128, ProviderError> {
        match self.ask(Call::Balance).await? {
            Reply::Balance(b) => b.parse().map_err(|_| other_answer()),
            _ => Err(other_answer()),
        }
    }

    async fn evidence(
        &self,
        lease_id: &str,
        query: &EvidenceQuery,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let secret = self.secret(lease_id).await?;
        self.chat.evidence(&secret, lease_id, query).await
    }
}

#[async_trait]
impl UsageSigner for RemoteProvider {
    async fn sign(&self, record: UsageRecord) -> Result<SignedUsage, String> {
        match self.ask(Call::SignUsage { record }).await {
            Ok(Reply::Signed(signed)) => Ok(signed),
            Ok(_) => Err("the key minter sent another answer".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}
