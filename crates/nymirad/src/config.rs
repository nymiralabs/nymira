//! `config.json` in the state directory: where things are, never a secret.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rpc::Transport;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub rpc_url: String,
    pub operator_url: String,
    /// The funding account (the CLI signs for it).
    pub account: String,
    /// The vault account; defaults to the pinned one.
    pub vault: Option<String>,
    /// `host:port` of a loopback SOCKS5 proxy (Tor). Without it every call is direct, which the
    /// CLI confirms with the user first.
    pub socks: Option<String>,
    /// Age public key of the backup recipient and the places the copies go.
    pub backup_recipient: Option<String>,
    #[serde(default)]
    pub backup_places: Vec<PathBuf>,
}

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

    pub fn transport(&self) -> Transport {
        match &self.socks {
            Some(addr) => Transport::Socks(addr.clone()),
            None => Transport::Direct,
        }
    }
}
