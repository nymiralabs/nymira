//! Follows the signed attestation policy while `nymirad` runs.
//!
//! NEAR AI redeploys its models from time to time and the signed policy then lists the new
//! deployment. The two web hosts serve the newest policy as `attestation-policy.json`. This
//! module fetches it through the same transport as every other request, checks it exactly as a
//! file placed in the state directory is checked (the pinned signer, the minimum version, a
//! strictly higher version than the one in force) and only then swaps the verifier and saves the
//! file. A fetch that fails, a file that does not verify and a version that does not rise change
//! nothing: the policy in force stays, so a hostile or stale host can at most delay an update.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::attest::{Attestor, ModelIdentity, Policy, ReportRequest, Trust, MIN_POLICY_VERSION};
use crate::rpc::{client, Transport};

/// The file in the state directory a newer policy is saved to.
pub const FILE: &str = "attestation-policy.json";
/// A signed policy is a few KiB; anything near this size is not one.
const MAX_BYTES: usize = 256 * 1024;

/// Builds the verifier for a checked policy, or says why this build cannot use it.
pub type Build = Box<dyn Fn(&Policy) -> Result<Arc<dyn Attestor>, String> + Send + Sync>;

pub struct Follower {
    home: PathBuf,
    signer: [u8; 32],
    build: Build,
    /// The version in force (0: none usable) and the verifier built from it.
    current: RwLock<(u32, Arc<dyn Attestor>)>,
}

impl Follower {
    pub fn new(
        home: &Path,
        signer: [u8; 32],
        version: u32,
        attestor: Arc<dyn Attestor>,
        build: Build,
    ) -> Arc<Self> {
        Arc::new(Self {
            home: home.to_path_buf(),
            signer,
            build,
            current: RwLock::new((version, attestor)),
        })
    }

    pub fn version(&self) -> u32 {
        self.current.read().expect("policy lock").0
    }

    fn attestor(&self) -> Arc<dyn Attestor> {
        self.current.read().expect("policy lock").1.clone()
    }

    /// Takes `bytes` as the policy in force when they verify and rise above the current version.
    /// `Ok(Some(version))` when the policy changed, `Ok(None)` when it did not rise.
    pub fn accept(&self, bytes: &[u8]) -> Result<Option<u32>, String> {
        let policy =
            Policy::load(bytes, &self.signer, MIN_POLICY_VERSION).map_err(|e| e.to_string())?;
        if policy.version <= self.version() {
            return Ok(None);
        }
        let attestor = (self.build)(&policy)?;
        save(&self.home.join(FILE), bytes).map_err(|e| format!("{FILE}: {e}"))?;
        *self.current.write().expect("policy lock") = (policy.version, attestor);
        Ok(Some(policy.version))
    }

    /// Asks each origin in turn for its policy; the first one that rises wins. Errors name the
    /// origin and what was wrong, one per origin that failed.
    pub async fn fetch_once(
        &self,
        transport: &Transport,
        origins: &[&str],
    ) -> Result<Option<u32>, Vec<String>> {
        let mut errors = Vec::new();
        for origin in origins {
            match self.fetch(transport, origin).await {
                Ok(Some(version)) => return Ok(Some(version)),
                Ok(None) => {}
                Err(e) => errors.push(format!("{origin}: {e}")),
            }
        }
        if errors.len() == origins.len() && !origins.is_empty() {
            Err(errors)
        } else {
            Ok(None)
        }
    }

    async fn fetch(&self, transport: &Transport, origin: &str) -> Result<Option<u32>, String> {
        // A client of its own: a fresh Tor circuit that carries nothing else.
        let http = client(transport).map_err(|e| e.to_string())?;
        let url = format!("{}/{FILE}", origin.trim_end_matches('/'));
        let mut res = http
            .get(url)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !res.status().is_success() {
            return Err(format!("status {}", res.status().as_u16()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| e.without_url().to_string())? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BYTES {
                return Err("the answer is too large to be a policy".into());
            }
        }
        self.accept(&body)
    }

    /// Checks at once and then every `every`, until the process ends. Direct transport is left
    /// alone: it would show this machine's address to the hosts without the user's question.
    pub fn spawn(self: &Arc<Self>, transport: Transport, origins: Vec<String>, every: Duration) {
        if matches!(transport, Transport::Direct) {
            return;
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                let list: Vec<&str> = origins.iter().map(String::as_str).collect();
                match me.fetch_once(&transport, &list).await {
                    Ok(Some(v)) => eprintln!("attestation policy updated to version {v}"),
                    Ok(None) => {}
                    Err(why) => eprintln!(
                        "warning: attestation policy not refreshed: {}",
                        why.join("; ")
                    ),
                }
            }
        });
    }
}

/// Writes `bytes` to `path` through a temporary file, so a crash leaves the old file or the new
/// one and never half of one.
fn save(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[async_trait]
impl Attestor for Follower {
    async fn verify_model(&self, ask: &ReportRequest<'_>) -> Result<ModelIdentity, String> {
        self.attestor().verify_model(ask).await
    }

    fn wants_operator_time(&self) -> bool {
        self.attestor().wants_operator_time()
    }

    fn verify_signature(
        &self,
        identity: &ModelIdentity,
        signature: &Value,
        model: &str,
        request: &[u8],
        response: &[u8],
    ) -> Trust {
        self.attestor()
            .verify_signature(identity, signature, model, request, response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::{Policy, Unavailable};
    use ed25519_dalek::SigningKey;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    fn signed(version: u32) -> Vec<u8> {
        serde_json::to_vec(
            &Policy {
                version,
                attestation_root_pk: "aa".into(),
                accepted_measurements: vec!["m1".into()],
                deployments: vec![],
                accepted_tcb_statuses: vec!["UpToDate".into()],
                nvidia_pins: vec![],
                signature: String::new(),
            }
            .sign(&key()),
        )
        .unwrap()
    }

    fn follower(home: &Path, version: u32) -> Arc<Follower> {
        Follower::new(
            home,
            key().verifying_key().to_bytes(),
            version,
            Arc::new(Unavailable),
            Box::new(|_| Ok(Arc::new(Unavailable))),
        )
    }

    #[test]
    fn a_higher_signed_policy_is_taken_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let f = follower(dir.path(), 5);
        assert_eq!(f.accept(&signed(9)).unwrap(), Some(9));
        assert_eq!(f.version(), 9);
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), signed(9));
    }

    #[test]
    fn an_equal_or_lower_version_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = follower(dir.path(), 9);
        assert_eq!(f.accept(&signed(9)).unwrap(), None);
        assert_eq!(f.accept(&signed(8)).unwrap(), None);
        assert_eq!(f.version(), 9);
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn a_policy_that_does_not_verify_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = follower(dir.path(), 5);
        let mut bad: Policy = serde_json::from_slice(&signed(10)).unwrap();
        bad.accepted_measurements.push("evil".into());
        assert!(f.accept(&serde_json::to_vec(&bad).unwrap()).is_err());
        let foreign = Policy {
            signature: String::new(),
            ..serde_json::from_slice(&signed(10)).unwrap()
        }
        .sign(&SigningKey::from_bytes(&[1; 32]));
        assert!(f.accept(&serde_json::to_vec(&foreign).unwrap()).is_err());
        assert!(f.accept(b"not json").is_err());
        assert_eq!(f.version(), 5);
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn a_policy_this_build_cannot_use_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = Follower::new(
            dir.path(),
            key().verifying_key().to_bytes(),
            5,
            Arc::new(Unavailable),
            Box::new(|_| Err("no deployment".into())),
        );
        assert!(f.accept(&signed(9)).is_err());
        assert_eq!(f.version(), 5);
        assert!(!dir.path().join(FILE).exists());
    }
}
