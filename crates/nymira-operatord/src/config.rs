//! The operator's configuration: one JSON file that every process reads. It names files; it
//! holds no secret. Each secret sits in its own 0400 file under `secrets_dir`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `sandbox`, `testnet` or `mainnet`. Only the sandbox may run without a replica or with
    /// development signing seeds.
    pub profile: String,
    /// `testnet` or `mainnet`: picks the chain id.
    pub network: String,
    pub vault_account: String,
    /// The account of the `heartbeat`, `mutual_close` and upkeep keys.
    pub operator_account: String,
    /// The separate account of the exit-answer key.
    pub answer_account: String,
    /// The host name clients dial; the status nonce is signed for it.
    pub host: String,
    pub web_origin: String,
    /// The one TCP listener: the public API.
    pub listen: String,
    pub run_dir: PathBuf,
    pub data_dir: PathBuf,
    pub secrets_dir: PathBuf,
    /// Directory of the verifying keys.
    pub setup_dir: PathBuf,
    /// RPC endpoints. The watcher needs at least as many as its quorum: 2 on testnet, 3 on mainnet.
    pub rpc_urls: Vec<String>,
    /// Mainnet only: an archival endpoint, which the regular providers are not. The profile is
    /// refused without one. Nothing polls it: it is for reading old receipts by hand (ceremony
    /// beacon, disputed payouts) and the config keeps it beside the providers.
    pub archival_rpc_url: Option<String>,
    /// The key that signs usage records, as hex; its secret half is in the key minter's file.
    pub usage_public_key: String,
    pub team_note_ids_file: Option<PathBuf>,
    /// `hackathon` (1 USD of credit in total) or `default`.
    #[serde(default = "hackathon")]
    pub budget: String,
    pub near_ai: NearAiSection,
    pub replica: Option<ReplicaSection>,
    /// Set on the second machine only: the status URL the dead-man switch watches.
    pub deadman_status_url: Option<String>,
    /// Set on the second machine only: where its replica node listens.
    pub replica_node: Option<ReplicaNodeSection>,
    #[serde(default)]
    pub alarms: AlarmSection,
}

fn hackathon() -> String {
    "hackathon".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearAiSection {
    pub base_url: String,
    pub org_id: String,
    pub workspace_id: String,
    /// Only for a first start: the refresh token goes in a 0400 file that is read once.
    pub first_refresh_token_file: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaSection {
    /// The replica's address on the tailnet.
    pub url: String,
    /// The replica's acknowledgement key, as hex.
    pub public_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaNodeSection {
    /// The tailnet address and port the laptop's replicator calls.
    pub listen: String,
    /// The operator's entry-signing key, as hex: entries without its signature are refused.
    pub entry_public_key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlarmSection {
    pub telegram: Option<TelegramSection>,
    pub email: Option<EmailSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSection {
    pub token_file: PathBuf,
    pub chat_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailSection {
    pub host: String,
    pub port: u16,
    /// `tls` or `starttls`.
    pub security: String,
    pub username: Option<String>,
    pub password_file: Option<PathBuf>,
    pub from: String,
    pub to: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: Self = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), String> {
        if !matches!(self.profile.as_str(), "sandbox" | "testnet" | "mainnet") {
            return Err("profile is sandbox, testnet or mainnet".into());
        }
        if !matches!(self.network.as_str(), "testnet" | "mainnet") {
            return Err("network is testnet or mainnet".into());
        }
        if self.rpc_urls.is_empty() {
            return Err("rpc_urls is empty".into());
        }
        if self.profile == "mainnet" {
            if self.rpc_urls.len() < self.watcher_quorum() {
                return Err(format!(
                    "the mainnet profile needs at least {} rpc_urls, one per watcher provider",
                    self.watcher_quorum()
                ));
            }
            if self.archival_rpc_url.is_none() {
                return Err("the mainnet profile needs an archival_rpc_url".into());
            }
        }
        if self.profile != "sandbox" && self.replica.is_none() && self.replica_node.is_none() {
            return Err("a testnet or mainnet operator needs a replica".into());
        }
        if self.profile == "mainnet" && self.network != "mainnet" {
            return Err("the mainnet profile runs on the mainnet network".into());
        }
        Ok(())
    }

    /// Providers that must report an exit before it counts: 3 on mainnet, 2 elsewhere.
    pub fn watcher_quorum(&self) -> usize {
        if self.profile == "mainnet" {
            3
        } else {
            2
        }
    }

    pub fn socket(&self, name: &str) -> PathBuf {
        self.run_dir.join(format!("{name}.sock"))
    }

    pub fn secret(&self, name: &str) -> PathBuf {
        self.secrets_dir.join(name)
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("operator.db")
    }

    pub fn sandbox(&self) -> bool {
        self.profile == "sandbox"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example file operators copy must stay loadable: every field name and the checks.
    #[test]
    fn the_example_config_loads() {
        let dir = std::env::temp_dir().join(format!("nymira-example-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("operator.json");
        std::fs::write(
            &path,
            include_str!("../../../scripts/operator/operator.example.json"),
        )
        .unwrap();
        let cfg = Config::load(&path).expect("the example config loads");
        assert_eq!(cfg.profile, "testnet");
        assert!(cfg.rpc_urls.len() >= 2);
        assert!(cfg.replica.is_some() && cfg.replica_node.is_none());
        // The second machine's file: the replica node and the dead-man switch read these.
        let path = dir.join("replica.json");
        std::fs::write(
            &path,
            include_str!("../../../scripts/operator/replica.example.json"),
        )
        .unwrap();
        let second = Config::load(&path).expect("the second machine's example config loads");
        assert!(second.replica_node.is_some() && second.deadman_status_url.is_some());
        assert!(second.rpc_urls.len() >= second.watcher_quorum());
        std::fs::remove_dir_all(dir).ok();
    }
}
