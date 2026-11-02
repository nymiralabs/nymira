//! `config.json` in the state directory: where things are, never a secret.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tor::{Backend, TorConfig};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub rpc_url: String,
    pub operator_url: String,
    /// The funding account (the CLI signs for it).
    pub account: String,
    /// The vault account; defaults to the pinned one.
    pub vault: Option<String>,
    /// `host:port` of a loopback SOCKS5 proxy someone else runs. Without it `nymirad` starts and
    /// manages Tor itself (see `tor_backend`). Must be a loopback address. Only a demo build
    /// with `direct` set goes out without Tor.
    pub socks: Option<String>,
    /// `tor` (the default) or `arti`.
    #[serde(default)]
    pub tor_backend: Option<String>,
    /// The package executable; defaults to `<home>/tor/package/<backend>`.
    #[serde(default)]
    pub tor_package: Option<PathBuf>,
    /// Demo builds only: the package hash to expect. A release build uses the one compiled in.
    #[serde(default)]
    pub tor_sha256: Option<String>,
    /// Demo builds only: talk to the sandbox without Tor. A release build ignores it.
    #[serde(default)]
    pub direct: bool,
    /// Age public key of the backup recipient and the places the copies go.
    pub backup_recipient: Option<String>,
    #[serde(default)]
    pub backup_places: Vec<PathBuf>,
    /// NEAR AI's public gateway (attestation reports, model list). Default below.
    #[serde(default)]
    pub gateway_url: Option<String>,
    /// Directory with the proving key and the other proof files.
    #[serde(default)]
    pub setup_dir: Option<String>,
    /// Demo builds only: the proving key hash to expect. A release build uses the one compiled in.
    #[serde(default)]
    pub proving_key_sha256: Option<String>,
    /// Seconds of silence after which a lease is retired. Default 120.
    #[serde(default)]
    pub idle_secs: Option<u64>,
}

pub const DEFAULT_GATEWAY: &str = "https://cloud-api.near.ai";

impl Config {
    pub fn load(home: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(home.join("config.json"))
            .map_err(|e| format!("config.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("config.json: {e}"))
    }

    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(home)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::write(
            home.join("config.json"),
            serde_json::to_vec_pretty(self).expect("config serializes"),
        )
    }

    /// How to start the managed Tor.
    pub fn tor_config(&self, home: &Path) -> Result<TorConfig, String> {
        let name = self.tor_backend.as_deref().unwrap_or("tor");
        let backend = Backend::parse(name)
            .ok_or_else(|| format!("tor_backend must be tor or arti, not {name}"))?;
        let sha256 = match (&self.tor_sha256, cfg!(feature = "mock")) {
            (Some(h), true) => h.clone(),
            _ => crate::tor::pinned_sha256(backend).to_string(),
        };
        Ok(TorConfig {
            backend,
            package: self
                .tor_package
                .clone()
                .unwrap_or_else(|| home.join("tor/package").join(backend.name())),
            sha256,
            work_dir: home.join("tor/run"),
        })
    }

    /// Demo builds may run without Tor; a release build never does.
    pub fn direct_allowed(&self) -> bool {
        self.direct && cfg!(feature = "mock")
    }
}
