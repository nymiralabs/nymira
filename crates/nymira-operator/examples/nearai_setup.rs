//! Setup and renewal of the NEAR AI login, with the settings in `~/.nymira-operator.env`.
//!
//! Exchanges the refresh token for a session token and writes the replacement refresh token
//! back at once, because the provider may kill the old one. Finds the organisation and workspace
//! ids and creates a read-only reporting token when the file has none. Prints ids' names, lengths
//! and lifetimes, never a token.
//!
//! The refresh token comes from `NEAR_AI_REFRESH_TOKEN` in the env file when that holds an `rt_`
//! token (a fresh browser login: copy the `refresh_token` cookie there), otherwise from the
//! sealed login file `~/.nymira-live/login.enc` (passphrase `NYMIRA_SEAL_PASSPHRASE`). The
//! rotated token goes back into the sealed file, and the env line is blanked, so one copy exists.
//! Run it before 2026-11-09 08:30 UTC, the expiry of the current token, and again to measure
//! whether each exchange extends it.
//!
//! `cargo run -p nymira-operator --example nearai_setup`

use std::collections::HashMap;
use std::io::Write;

use base64::Engine;
use nymira_operator::nearai::parse_rfc3339;
use nymira_operator::sealed::SealedFile;
use serde_json::{json, Value};

const BASE: &str = "https://cloud-api.near.ai";

struct Env {
    path: String,
    lines: Vec<String>,
}

impl Env {
    fn load() -> Self {
        let path = format!("{}/.nymira-operator.env", std::env::var("HOME").unwrap());
        let text = std::fs::read_to_string(&path).expect("settings file");
        Self {
            path,
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    fn map(&self) -> HashMap<String, String> {
        self.lines
            .iter()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().into(), v.trim().into()))
            .collect()
    }

    /// Sets a key and rewrites the file atomically, mode 0600.
    fn set(&mut self, key: &str, value: &str) {
        let line = format!("{key}={value}");
        match self
            .lines
            .iter_mut()
            .find(|l| l.starts_with(&format!("{key}=")))
        {
            Some(l) => *l = line,
            None => self.lines.push(line),
        }
        let tmp = format!("{}.tmp", self.path);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).expect("temp file");
        writeln!(file, "{}", self.lines.join("\n")).expect("write");
        file.sync_all().expect("sync");
        std::fs::rename(&tmp, &self.path).expect("rename");
    }
}

async fn get(http: &reqwest::Client, token: &str, path: &str) -> Result<Value, String> {
    let reply = http
        .get(format!("{BASE}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !reply.status().is_success() {
        return Err(format!("{path}: status {}", reply.status().as_u16()));
    }
    reply.json().await.map_err(|e| e.without_url().to_string())
}

/// An error text with anything that looks like a token or a long secret replaced.
fn masked(text: &str) -> String {
    text.chars()
        .take(300)
        .collect::<String>()
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .map(|w| if w.len() > 24 { "<masked>" } else { w })
        .collect::<Vec<_>>()
        .join(" ")
}

fn jwt_times(token: &str) -> Option<(u64, u64)> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    Some((v["iat"].as_u64()?, v["exp"].as_u64()?))
}

#[tokio::main]
async fn main() {
    let mut env = Env::load();
    let settings = env.map();
    let sealed = settings.get("NYMIRA_SEAL_PASSPHRASE").and_then(|pass| {
        let dir = format!("{}/.nymira-live", std::env::var("HOME").unwrap());
        SealedFile::open(format!("{dir}/login.enc").as_ref(), pass, 600_000)
            .map_err(|e| println!("sealed login file not opened: {e}"))
            .ok()
    });
    let stored: Option<Value> = sealed
        .as_ref()
        .and_then(|f| f.read().ok().flatten())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let from_env = settings
        .get("NEAR_AI_REFRESH_TOKEN")
        .filter(|t| t.starts_with("rt_"))
        .cloned();
    let replacing = from_env.is_some();
    let refresh = from_env
        .or_else(|| {
            stored
                .as_ref()
                .and_then(|l| l["refresh_token"].as_str().map(String::from))
        })
        .unwrap_or_default();
    if let Some(l) = stored.as_ref().filter(|_| !replacing) {
        println!(
            "sealed login: expires {}",
            l["expires_at"]
                .as_u64()
                .map_or("unknown".into(), |t| t.to_string())
        );
    }
    let http = reqwest::Client::builder()
        .user_agent(nymira_operator::nearai::BROWSER_USER_AGENT)
        .build()
        .expect("client");
    let given = settings
        .get("NEAR_AI_ACCESS_TOKEN")
        .filter(|t| t.starts_with("eyJ"))
        .cloned();

    // A session token copied from the browser stands in for the exchange, which this account's
    // refresh token has refused so far. It is erased from the file as soon as it is read.
    let access: String = match given {
        Some(token) => {
            println!("using the session token from the file");
            env.set("NEAR_AI_ACCESS_TOKEN", "");
            token
        }
        None => {
            if !refresh.starts_with("rt_") {
                return println!("NEAR_AI_REFRESH_TOKEN is not a refresh token (no rt_ prefix)");
            }
            let reply = http
                .post(format!("{BASE}/v1/users/me/access-tokens"))
                .bearer_auth(&refresh)
                .send()
                .await;
            let reply = match reply {
                Ok(r) => r,
                Err(e) => return println!("exchange failed: {}", e.without_url()),
            };
            if !reply.status().is_success() {
                let status = reply.status().as_u16();
                let text = reply.text().await.unwrap_or_default();
                return println!("exchange refused: status {status}: {}", masked(&text));
            }
            let body: Value = reply.json().await.expect("json");
            let (Some(access), Some(new_refresh)) = (
                body["access_token"].as_str(),
                body["refresh_token"].as_str(),
            ) else {
                return println!("exchange reply has no tokens");
            };
            // Before anything else: the old refresh token may be dead now.
            let rotated = new_refresh != refresh;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let expires_at = body["refresh_token_expiration"]
                .as_str()
                .and_then(parse_rfc3339)
                .or_else(|| {
                    stored
                        .as_ref()
                        .and_then(|l| l["expires_at"].as_u64())
                        .filter(|_| !replacing && !rotated)
                });
            let issued_at = match stored.as_ref().filter(|_| !replacing && !rotated) {
                Some(l) => l["issued_at"].as_u64().unwrap_or(now),
                None => now,
            };
            match &sealed {
                Some(file) => {
                    let login = json!({
                        "refresh_token": new_refresh,
                        "expires_at": expires_at,
                        "issued_at": issued_at,
                    });
                    file.write(&serde_json::to_vec(&login).expect("login serializes"))
                        .expect("sealed login written");
                    if replacing {
                        env.set("NEAR_AI_REFRESH_TOKEN", "");
                    }
                    println!("refresh token stored in the sealed login file");
                }
                None if rotated => env.set("NEAR_AI_REFRESH_TOKEN", new_refresh),
                None => {}
            }
            println!("refresh token rotated on exchange: {rotated}");
            println!(
                "refresh token expires: {}",
                body["refresh_token_expiration"]
                    .as_str()
                    .unwrap_or("not reported")
            );
            access.to_string()
        }
    };
    let access = access.as_str();
    match jwt_times(access) {
        Some((iat, exp)) => println!("session token lifetime: {} s", exp - iat),
        None => println!("session token has no readable iat/exp"),
    }

    // The account's refresh tokens: times and the client that made them, no secrets.
    match get(&http, access, "/v1/users/me/refresh-tokens").await {
        Ok(v) => {
            let list = v.as_array().cloned().unwrap_or_default();
            println!("refresh tokens on the account: {}", list.len());
            for t in &list {
                println!(
                    "  created {} expires {} agent {}",
                    t["created_at"].as_str().unwrap_or("?"),
                    t["expires_at"].as_str().unwrap_or("?"),
                    t["user_agent"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(40)
                        .collect::<String>()
                );
            }
        }
        Err(e) => println!("refresh token list: {e}"),
    }

    let orgs = match get(&http, access, "/v1/organizations").await {
        Ok(v) => v,
        Err(e) => return println!("organisations: {e}"),
    };
    let list = orgs["organizations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for o in &list {
        println!(
            "organisation: {} (role {}, id length {})",
            o["name"].as_str().unwrap_or("?"),
            o["role"].as_str().unwrap_or("?"),
            o["id"].as_str().map_or(0, str::len)
        );
    }
    let wanted = settings.get("NEAR_AI_ORG_ID").cloned();
    let org = list
        .iter()
        .find(|o| wanted.as_deref().is_some_and(|w| o["id"] == w))
        .or_else(|| (list.len() == 1).then(|| &list[0]));
    let Some(org) = org else {
        return println!(
            "pick one: put its id into NEAR_AI_ORG_ID and run again (or the list is empty)"
        );
    };
    let org_id = org["id"].as_str().expect("id").to_string();
    env.set("NEAR_AI_ORG_ID", &org_id);

    let workspaces = match get(
        &http,
        access,
        &format!("/v1/organizations/{org_id}/workspaces"),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return println!("workspaces: {e}"),
    };
    let list = workspaces["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for w in &list {
        println!("workspace: {}", w["name"].as_str().unwrap_or("?"));
    }
    let wanted = settings.get("NEAR_AI_WORKSPACE_ID").cloned();
    let ws = list
        .iter()
        .find(|w| wanted.as_deref().is_some_and(|x| w["id"] == x))
        .or_else(|| (list.len() == 1).then(|| &list[0]));
    match ws {
        Some(w) => env.set("NEAR_AI_WORKSPACE_ID", w["id"].as_str().expect("id")),
        None => println!("pick a workspace: put its id into NEAR_AI_WORKSPACE_ID and run again"),
    }

    let have = settings
        .get("NEAR_AI_REPORTING_TOKEN")
        .is_some_and(|t| t.starts_with("rpt-") && t.len() > 20);
    if have {
        return println!("reporting token: kept");
    }
    let created = http
        .post(format!("{BASE}/v1/organizations/{org_id}/reporting-tokens"))
        .bearer_auth(access)
        .json(&serde_json::json!({"name": "nymira-operator"}))
        .send()
        .await;
    match created {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.expect("json");
            match v["token"].as_str() {
                Some(t) => {
                    env.set("NEAR_AI_REPORTING_TOKEN", t);
                    println!("reporting token: created, length {}", t.len());
                }
                None => println!("reporting token: reply has no token"),
            }
        }
        Ok(r) => println!("reporting token refused: status {}", r.status().as_u16()),
        Err(e) => println!("reporting token failed: {}", e.without_url()),
    }
}
