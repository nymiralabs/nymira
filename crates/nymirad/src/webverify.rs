//! `nymirad verify-web`: checks what the web hosts serve against the signed release manifest.
//! The rules (what is hashed, in what form, which headers) are in `web/INTEGRITY.md`; this code
//! must give the same bytes and hashes as its JavaScript reference.
//!
//! Each of the four fetches (two origins, over Tor and directly) is checked alone against the
//! manifest, and the four are then compared with each other. The key and the origins are compiled
//! in; nothing here reads them from the user. The direct fetch shows the user's address to the
//! hosts, so the caller passes whether the user agreed, and without it no direct request is made.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::rpc::Transport;

/// The offline release key (minisign public key file). Empty until the project key exists.
pub const MANIFEST_KEY: &str = include_str!("../pins/web-manifest.pub");
/// Two lines: the primary origin (sends the signed headers), then the mirror. Empty until the
/// project domain exists.
pub const ORIGINS: &str = include_str!("../pins/web-origins.txt");

/// Signed header names, in the order of the header set file.
pub const SIGNED_HEADERS: [&str; 6] = [
    "access-control-allow-origin",
    "content-security-policy",
    "permissions-policy",
    "referrer-policy",
    "strict-transport-security",
    "x-content-type-options",
];
const CLASSES: [&str; 3] = ["app", "assets", "frame"];
/// An unknown path, fetched to read `404.html` (served only with status 404).
const UNKNOWN_PATH: &str = "nymira-verify-404";
const MAX_BODY: usize = 16 << 20;

/// The question before the direct fetch. Fixed wording.
pub const DIRECT_QUESTION: &str = "verify-web now fetches the site a second time without Tor. That shows your IP address, the time and the files requested to the two hosts (the primary origin and the mirror). Type yes to fetch directly, or anything else to skip it; the check is then incomplete and exits with an error.";

/// What this check cannot see. Printed with every report.
pub const LIMITS: &str = "What this cannot detect: a host that serves this client the signed files and other visitors something else (cloaking). Tor and a second host make that harder, not impossible. It also cannot see files that are not in the signed listing, and it does not tell you what the code does.";

pub struct Plan<'a> {
    /// The minisign public key file text.
    pub key: &'a str,
    /// `testnet` or `mainnet`: the profile the trusted comment must name.
    pub profile: &'a str,
    /// Checked for headers as well as bytes.
    pub primary: &'a str,
    /// Bytes only: a static mirror cannot send the headers.
    pub mirror: &'a str,
}

/// The key and origins of this build, or why there are none.
pub fn compiled() -> Result<(&'static str, Vec<&'static str>), String> {
    let origins: Vec<&str> = ORIGINS.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if MANIFEST_KEY.trim().is_empty() || origins.len() != 2 {
        return Err("this build pins no web manifest key or no web origins, so verify-web cannot run (a release build fills pins/web-manifest.pub and pins/web-origins.txt)".into());
    }
    Ok((MANIFEST_KEY, origins))
}

// ---- hashes and formats -------------------------------------------------------------------

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// `<sha256>  <path>\n` per file, paths in byte order: what `sha256sum` prints.
pub fn bundle_listing(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    sorted
        .iter()
        .map(|(path, data)| format!("{}  {path}\n", sha256_hex(data)))
        .collect()
}

pub type HeaderSet = BTreeMap<String, BTreeMap<String, String>>;

/// `# class` blocks of `name: value` lines, signed names only, in the fixed order.
pub fn canonical_header_set(set: &HeaderSet) -> String {
    let mut out = String::new();
    for class in CLASSES {
        out += &format!("# {class}\n");
        if let Some(headers) = set.get(class) {
            for name in SIGNED_HEADERS {
                if let Some(value) = headers.get(name) {
                    out += &format!("{name}: {value}\n");
                }
            }
        }
    }
    out
}

/// The inverse of [`canonical_header_set`]; refuses any text that is not in that form.
pub fn parse_header_set(text: &str) -> Result<HeaderSet, String> {
    let mut set = HeaderSet::new();
    let mut current: Option<String> = None;
    for line in text.split('\n').filter(|l| !l.is_empty()) {
        if let Some(class) = line.strip_prefix("# ") {
            set.insert(class.to_string(), BTreeMap::new());
            current = Some(class.to_string());
        } else {
            let (name, value) = line
                .split_once(": ")
                .ok_or_else(|| format!("header set: bad line {line:?}"))?;
            let class = current.as_ref().ok_or("header set: a header before any class")?;
            set.get_mut(class)
                .expect("class was inserted")
                .insert(name.to_string(), value.to_string());
        }
    }
    if canonical_header_set(&set) != text {
        return Err("integrity/headers.txt is not in the canonical form".into());
    }
    Ok(set)
}

pub fn class_of_path(path: &str) -> &'static str {
    if path == "/wallet-host.html" || path == "/wallet-host" {
        "frame"
    } else if path.starts_with("/assets/") || path.starts_with("/executors/") {
        "assets"
    } else {
        "app"
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub bundle_hash: String,
    pub header_set_hash: String,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parses `manifest.json`, accepting only the canonical bytes: the signature covers the bytes, so
/// a reader that parsed and re-serialized could be fooled by duplicate keys.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("manifest: {e}"))?;
    if !is_hex64(&m.bundle_hash) || !is_hex64(&m.header_set_hash) {
        return Err("manifest: hashes must be 64 lowercase hex characters".into());
    }
    let canonical = format!("{}\n", serde_json::to_string_pretty(&m).expect("serializes"));
    if canonical.as_bytes() != bytes {
        return Err("manifest: not in the canonical form".into());
    }
    Ok(m)
}

/// A bundle path the listing may name: ASCII, no way out of the site, nothing a URL would
/// reinterpret.
fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.ends_with('/')
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b))
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

fn parse_listing(text: &str) -> Result<Vec<(String, String)>, String> {
    let body = text
        .strip_suffix('\n')
        .ok_or("integrity/files.txt must end with a newline")?;
    let mut out: Vec<(String, String)> = Vec::new();
    for line in body.split('\n') {
        let (hash, path) = match (line.get(..64), line.get(64..66), line.get(66..)) {
            (Some(h), Some("  "), Some(p)) if is_hex64(h) && safe_path(p) => (h, p),
            _ => return Err(format!("integrity/files.txt: bad line {line:?}")),
        };
        if out.last().is_some_and(|(_, last)| last.as_bytes() >= path.as_bytes()) {
            return Err("integrity/files.txt is not sorted or repeats a path".into());
        }
        out.push((hash.to_string(), path.to_string()));
    }
    Ok(out)
}

// ---- minisign -----------------------------------------------------------------------------

fn b64(s: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("base64: {e}"))
}

/// Verifies a minisign signature (prehashed `ED`, both signatures) and returns the trusted
/// comment. `Ed` (legacy) signatures are refused.
pub fn verify_minisign(msg: &[u8], sig_file: &str, pub_file: &str) -> Result<String, String> {
    let pub_lines: Vec<&str> = pub_file.split('\n').collect();
    let pk = b64(pub_lines.get(1).ok_or("public key: too short")?)?;
    if pk.len() != 42 || &pk[..2] != b"Ed" {
        return Err("public key: not a minisign Ed25519 key".into());
    }
    let key = VerifyingKey::from_bytes(pk[10..].try_into().expect("32 bytes"))
        .map_err(|e| format!("public key: {e}"))?;

    let lines: Vec<&str> = sig_file.split('\n').collect();
    if lines.len() < 4 || !lines[0].starts_with("untrusted comment:") {
        return Err("signature file: not a minisign signature".into());
    }
    let sig = b64(lines[1])?;
    if sig.len() != 74 || &sig[..2] != b"ED" {
        return Err("signature: not a prehashed (ED) minisign signature".into());
    }
    if sig[2..10] != pk[2..10] {
        return Err("signature: made by another key".into());
    }
    let comment = lines[2]
        .strip_prefix("trusted comment: ")
        .ok_or("signature file: no trusted comment")?;
    let global = b64(lines[3])?;
    let bad = |what: &str| format!("signature: {what} does not verify");
    let first = Signature::from_slice(&sig[10..]).map_err(|_| bad("the file signature"))?;
    key.verify_strict(&Blake2b512::digest(msg), &first)
        .map_err(|_| bad("the file signature"))?;
    let second = Signature::from_slice(&global).map_err(|_| bad("the comment signature"))?;
    let mut covered = sig[10..].to_vec();
    covered.extend(comment.as_bytes());
    key.verify_strict(&covered, &second)
        .map_err(|_| bad("the comment signature"))?;
    Ok(comment.to_string())
}

// ---- fetching -----------------------------------------------------------------------------

struct Resp {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

/// GETs `base + path`. A redirect is followed only inside the same origin and path prefix (Pages
/// moves `/wallet-host.html` to `/wallet-host`); any other redirect is an error.
async fn get(client: &reqwest::Client, base: &reqwest::Url, path: &str) -> Result<Resp, String> {
    let mut url = base
        .join(path)
        .map_err(|e| format!("{path}: bad path: {e}"))?;
    for _ in 0..4 {
        let mut resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| format!("{path}: {}", e.without_url()))?;
        let status = resp.status().as_u16();
        if resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .filter(|n| {
                    n.scheme() == base.scheme()
                        && n.host_str() == base.host_str()
                        && n.port_or_known_default() == base.port_or_known_default()
                        && n.path().starts_with(base.path())
                })
                .ok_or_else(|| format!("{path}: redirected outside the site (status {status})"))?;
            url = next;
            continue;
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{path}: {e}"))? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BODY {
                return Err(format!("{path}: larger than {MAX_BODY} bytes"));
            }
        }
        return Ok(Resp {
            status,
            headers: resp.headers().clone(),
            body,
        });
    }
    Err(format!("{path}: too many redirects"))
}

async fn get_ok(c: &reqwest::Client, base: &reqwest::Url, path: &str) -> Result<Vec<u8>, String> {
    let r = get(c, base, path).await?;
    if r.status != 200 {
        return Err(format!("{path}: status {}", r.status));
    }
    Ok(r.body)
}

/// What one fetch of one origin saw, for the comparison between fetches.
#[derive(Debug, Default, PartialEq, Eq)]
struct Seen {
    manifest: String,
    files: String,
    headers_file: String,
    /// path -> SHA-256 of the bytes served.
    served: BTreeMap<String, String>,
    /// probe path -> the six signed headers as served.
    header_probes: BTreeMap<String, String>,
}

pub struct Run {
    pub origin: String,
    pub route: &'static str,
    /// The version the signature vouches for, when it verified.
    pub version: Option<String>,
    pub files: usize,
    pub headers_checked: bool,
    pub problems: Vec<String>,
    seen: Seen,
}

impl Run {
    fn failed(origin: &str, route: &'static str, why: String) -> Self {
        Self {
            origin: origin.into(),
            route,
            version: None,
            files: 0,
            headers_checked: false,
            problems: vec![why],
            seen: Seen::default(),
        }
    }
}

/// Fetches and checks one origin through one client.
async fn check_origin(
    client: &reqwest::Client,
    origin: &str,
    route: &'static str,
    plan: &Plan<'_>,
    headers: bool,
) -> Run {
    let fail = |why: String| Run::failed(origin, route, why);
    let base = match reqwest::Url::parse(&format!("{}/", origin.trim_end_matches('/'))) {
        Ok(u) if matches!(u.scheme(), "https" | "http") => u,
        _ => return fail(format!("{origin}: not an http(s) URL")),
    };
    let mut run = Run::failed(origin, route, String::new());
    run.problems.clear();
    macro_rules! need {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => {
                    run.problems.push(e);
                    return run;
                }
            }
        };
    }
    // The manifest and its signature come first: nothing below is trusted before they verify.
    let manifest_bytes = need!(get_ok(client, &base, "manifest.json").await);
    let sig = need!(get_ok(client, &base, "manifest.json.minisig").await);
    let sig = need!(String::from_utf8(sig).map_err(|_| "manifest.json.minisig: not UTF-8".to_string()));
    let comment = need!(verify_minisign(&manifest_bytes, &sig, plan.key));
    let manifest = need!(parse_manifest(&manifest_bytes));
    let want = format!(
        "nymira-web-manifest v1 profile={} version={}",
        plan.profile, manifest.version
    );
    if comment != want {
        run.problems
            .push(format!("trusted comment is {comment:?}, expected {want:?}"));
        return run;
    }
    run.version = Some(manifest.version.clone());
    run.seen.manifest = sha256_hex(&manifest_bytes);

    let files_txt = need!(get_ok(client, &base, "integrity/files.txt").await);
    let headers_txt = need!(get_ok(client, &base, "integrity/headers.txt").await);
    run.seen.files = sha256_hex(&files_txt);
    run.seen.headers_file = sha256_hex(&headers_txt);
    if run.seen.files != manifest.bundle_hash {
        run.problems
            .push("integrity/files.txt does not match bundle_hash".into());
    }
    if run.seen.headers_file != manifest.header_set_hash {
        run.problems
            .push("integrity/headers.txt does not match header_set_hash".into());
    }
    if !run.problems.is_empty() {
        return run;
    }
    let listing = need!(String::from_utf8(files_txt).map_err(|_| "files.txt: not UTF-8".to_string()));
    let listing = need!(parse_listing(&listing));
    run.files = listing.len();
    for (hash, path) in &listing {
        let (url_path, status) = match path.as_str() {
            "index.html" => ("", 200),
            "404.html" => (UNKNOWN_PATH, 404),
            p => (p, 200),
        };
        match get(client, &base, url_path).await {
            Ok(r) if r.status == status => {
                let got = sha256_hex(&r.body);
                if &got != hash {
                    run.problems.push(format!("{path}: the served bytes differ from the listing"));
                }
                run.seen.served.insert(path.clone(), got);
            }
            Ok(r) => run.problems.push(format!("{path}: status {}, expected {status}", r.status)),
            Err(e) => run.problems.push(e),
        }
    }
    if headers {
        let text = need!(String::from_utf8(headers_txt).map_err(|_| "headers.txt: not UTF-8".to_string()));
        let set = need!(parse_header_set(&text));
        let pick = |dir: &str| listing.iter().find(|(_, p)| p.starts_with(dir)).map(|(_, p)| format!("/{p}"));
        let mut probes = vec!["/".to_string(), "/wallet-host.html".into(), "/wallet-host".into()];
        probes.extend(pick("assets/"));
        probes.extend(pick("executors/"));
        for probe in probes {
            let r = match get(client, &base, probe.trim_start_matches('/')).await {
                Ok(r) => r,
                Err(e) => {
                    run.problems.push(e);
                    continue;
                }
            };
            let class = set.get(class_of_path(&probe));
            let mut seen = Vec::new();
            for name in SIGNED_HEADERS {
                let got = join_values(&r.headers, name);
                let want = class.and_then(|c| c.get(name));
                if got.as_ref() != want {
                    run.problems.push(format!(
                        "{probe}: {name} is {}, the signed set says {}",
                        got.as_deref().unwrap_or("(none)"),
                        want.map_or("(none)", |s| s.as_str())
                    ));
                }
                seen.push(format!("{name}={}", got.unwrap_or_default()));
            }
            run.seen.header_probes.insert(probe, seen.join("\n"));
        }
        run.headers_checked = true;
    }
    run
}

fn join_values(h: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    let all: Vec<&str> = h.get_all(name).iter().filter_map(|v| v.to_str().ok()).collect();
    (!all.is_empty()).then(|| all.join(", "))
}

// ---- the whole check ----------------------------------------------------------------------

pub struct Report {
    pub runs: Vec<Run>,
    /// Differences between fetches.
    pub disagreements: Vec<String>,
    /// Parts that were not run.
    pub skipped: Vec<String>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.skipped.is_empty()
            && self.disagreements.is_empty()
            && self.runs.iter().all(|r| r.problems.is_empty())
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "{:<38} {:<7} {:<16} {:>5}  {:<8} {}\n",
            "origin", "route", "version", "files", "headers", "result"
        );
        for r in &self.runs {
            out += &format!(
                "{:<38} {:<7} {:<16} {:>5}  {:<8} {}\n",
                r.origin,
                r.route,
                r.version.as_deref().unwrap_or("-"),
                r.files,
                if r.headers_checked { "checked" } else { "-" },
                if r.problems.is_empty() { "ok" } else { "FAIL" }
            );
        }
        for r in &self.runs {
            for p in &r.problems {
                out += &format!("  {} via {}: {p}\n", r.origin, r.route);
            }
        }
        for d in &self.disagreements {
            out += &format!("  DISAGREEMENT: {d}\n");
        }
        for s in &self.skipped {
            out += &format!("  NOT RUN: {s}\n");
        }
        out += if self.ok() {
            "All fetches match the signed manifest and each other.\n"
        } else {
            "The check FAILED or is incomplete. Do not trust this site until it is explained.\n"
        };
        out + LIMITS + "\n"
    }
}

fn compare(a: &Run, b: &Run, out: &mut Vec<String>) {
    let who = format!("{} via {} and {} via {}", a.origin, a.route, b.origin, b.route);
    let both = !a.seen.manifest.is_empty() && !b.seen.manifest.is_empty();
    if both && a.seen.manifest != b.seen.manifest {
        out.push(format!("manifest.json differs between {who}"));
    }
    for (path, hash) in &a.seen.served {
        if b.seen.served.get(path).is_some_and(|other| other != hash) {
            out.push(format!("{path} differs between {who}"));
        }
    }
    if a.headers_checked && b.headers_checked && a.seen.header_probes != b.seen.header_probes {
        out.push(format!("served headers differ between {who}"));
    }
}

/// Runs the whole check. `tor` carries the Tor leg and must not be a direct transport.
/// `direct_confirmed` is the user's answer to [`DIRECT_QUESTION`]; without it no direct request
/// is made and the report is incomplete.
pub async fn verify(plan: &Plan<'_>, tor: &Transport, direct_confirmed: bool) -> Report {
    let mut runs = Vec::new();
    let mut skipped = Vec::new();
    let origins = [(plan.primary, true), (plan.mirror, false)];
    for (origin, headers) in origins {
        let client = if tor.is_direct() {
            Err("the Tor leg was given a direct transport".to_string())
        } else {
            crate::rpc::client(tor).map_err(|e| e.to_string())
        };
        runs.push(match client {
            Ok(c) => check_origin(&c, origin, "tor", plan, headers).await,
            Err(e) => Run::failed(origin, "tor", e),
        });
    }
    if direct_confirmed {
        let built = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build();
        for (origin, headers) in origins {
            runs.push(match &built {
                Ok(c) => check_origin(c, origin, "direct", plan, headers).await,
                Err(e) => Run::failed(origin, "direct", e.to_string()),
            });
        }
    } else {
        skipped.push("the direct fetch (not confirmed)".into());
    }
    let mut disagreements = Vec::new();
    for i in 0..runs.len() {
        for j in i + 1..runs.len() {
            compare(&runs[i], &runs[j], &mut disagreements);
        }
    }
    Report {
        runs,
        disagreements,
        skipped,
    }
}
