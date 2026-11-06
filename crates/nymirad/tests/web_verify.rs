//! `nymirad verify-web` against the signed fixture site (a copy of the web hosting work's test
//! fixture) served by local stub hosts, with a fake SOCKS proxy as the Tor leg.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::Engine;
use nymirad::rpc::Transport;
use nymirad::webverify::*;
use support::socks::{Behavior, FakeSocks};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/web-integrity");

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURE).join(rel)).unwrap()
}
fn text(rel: &str) -> String {
    String::from_utf8(read(rel)).unwrap()
}
fn expected() -> serde_json::Value {
    serde_json::from_slice(&read("expected.json")).unwrap()
}
fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let path = if rel.is_empty() {
            e.file_name().to_string_lossy().into_owned()
        } else {
            format!("{rel}/{}", e.file_name().to_string_lossy())
        };
        if e.path().is_dir() {
            walk(&e.path(), &path, out);
        } else {
            out.push((path, std::fs::read(e.path()).unwrap()));
        }
    }
}
fn copy_site() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    walk(&Path::new(FIXTURE).join("site"), "", &mut files);
    for (path, data) in files {
        let to = tmp.path().join(&path);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(to, data).unwrap();
    }
    tmp
}

// ---- hashes equal the JavaScript reference's ------------------------------------------------

#[test]
fn the_bundle_listing_and_both_hashes_equal_the_fixtures() {
    let mut bundle = Vec::new();
    walk(&Path::new(FIXTURE).join("src/bundle"), "", &mut bundle);
    let listing = bundle_listing(&bundle);
    assert_eq!(listing, text("site/integrity/files.txt"));
    let want = expected();
    assert_eq!(
        sha256_hex(listing.as_bytes()),
        want["manifest"]["bundle_hash"]
    );
    let headers = text("site/integrity/headers.txt");
    assert_eq!(
        sha256_hex(headers.as_bytes()),
        want["manifest"]["header_set_hash"]
    );
    // The header set parses and writes back to the same bytes.
    let set = parse_header_set(&headers).unwrap();
    assert_eq!(canonical_header_set(&set), headers);
    assert_eq!(bundle.len() as u64, want["files"].as_u64().unwrap());
}

#[test]
fn the_manifest_is_read_in_its_canonical_form_only() {
    let bytes = read("site/manifest.json");
    let m = parse_manifest(&bytes).unwrap();
    assert_eq!(m.bundle_hash, expected()["manifest"]["bundle_hash"]);
    let mut spaced = bytes.clone();
    spaced.pop();
    assert!(parse_manifest(&spaced).is_err(), "no trailing newline");
    let reordered = br#"{"bundle_hash":"","version":"x","header_set_hash":""}"#;
    assert!(parse_manifest(reordered).is_err());
    let doubled = String::from_utf8(bytes)
        .unwrap()
        .replace("\"version\"", "\"version\": \"y\",\n  \"version\"");
    assert!(parse_manifest(doubled.as_bytes()).is_err(), "duplicate key");
}

// ---- the signature --------------------------------------------------------------------------

#[test]
fn the_fixture_signature_verifies_and_any_change_is_refused() {
    let key = text("minisign.pub");
    let sig = text("site/manifest.json.minisig");
    let msg = read("site/manifest.json");
    let comment = verify_minisign(&msg, &sig, &key).unwrap();
    assert_eq!(comment, expected()["trusted_comment"]);

    let mut changed = msg.clone();
    changed[10] ^= 1;
    assert!(
        verify_minisign(&changed, &sig, &key).is_err(),
        "a changed byte"
    );

    let tampered_comment = sig.replace("version=0.0.1-fixture", "version=9.9.9");
    assert!(verify_minisign(&msg, &tampered_comment, &key).is_err());

    // Another key: flip a bit of the public key bytes.
    let lines: Vec<&str> = key.split('\n').collect();
    let mut pk = base64::engine::general_purpose::STANDARD
        .decode(lines[1])
        .unwrap();
    pk[20] ^= 1;
    let other = format!(
        "{}\n{}\n",
        lines[0],
        base64::engine::general_purpose::STANDARD.encode(&pk)
    );
    assert!(verify_minisign(&msg, &sig, &other).is_err());
    // Same key id, different key bytes is refused by the signature itself; a different id by the id.
    pk[20] ^= 1;
    pk[2] ^= 1;
    let other_id = format!(
        "{}\n{}\n",
        lines[0],
        base64::engine::general_purpose::STANDARD.encode(&pk)
    );
    assert!(verify_minisign(&msg, &sig, &other_id)
        .unwrap_err()
        .contains("another key"));

    // A legacy `Ed` signature is refused.
    let sl: Vec<&str> = sig.split('\n').collect();
    let mut raw = base64::engine::general_purpose::STANDARD
        .decode(sl[1])
        .unwrap();
    raw[1] = b'd';
    let legacy = format!(
        "{}\n{}\n{}\n{}\n",
        sl[0],
        base64::engine::general_purpose::STANDARD.encode(&raw),
        sl[2],
        sl[3]
    );
    assert!(verify_minisign(&msg, &legacy, &key).is_err());

    assert!(verify_minisign(&msg, "", &key).is_err());
}

// ---- stub hosts -----------------------------------------------------------------------------

struct Stub {
    dir: PathBuf,
    prefix: &'static str,
    /// Send the signed headers (the primary origin).
    headers: bool,
    /// Drop this signed header from every response.
    drop_header: Option<&'static str>,
    /// (class, header, value) set on top of the signed headers, as a host that adds its own.
    extra: Vec<(&'static str, &'static str, &'static str)>,
    /// From this many `manifest.json` requests on, serve different bytes for `assets/app.js`.
    tamper_after: Option<usize>,
    manifests: AtomicUsize,
    hits: AtomicUsize,
}

async fn serve(State(stub): State<Arc<Stub>>, uri: Uri) -> Response {
    stub.hits.fetch_add(1, Ordering::SeqCst);
    let Some(path) = uri.path().strip_prefix(stub.prefix) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut path = if path.is_empty() { "index.html" } else { path };
    if path == "manifest.json" {
        stub.manifests.fetch_add(1, Ordering::SeqCst);
    }
    let class = class_of_path(&format!("/{path}"));
    if path == "wallet-host" {
        path = "wallet-host.html";
    }
    let Ok(mut body) = std::fs::read(stub.dir.join(path)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if path == "assets/app.js"
        && stub
            .tamper_after
            .is_some_and(|n| stub.manifests.load(Ordering::SeqCst) >= n)
    {
        body.push(b'!');
    }
    let mut resp = body.into_response();
    if stub.headers {
        let set = parse_header_set(
            &std::fs::read_to_string(stub.dir.join("integrity/headers.txt")).unwrap(),
        )
        .unwrap();
        for (name, value) in &set[class] {
            if stub.drop_header != Some(name.as_str()) {
                resp.headers_mut().insert(
                    HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
        }
    }
    if stub.headers {
        for (extra_class, name, value) in &stub.extra {
            if *extra_class == class {
                resp.headers_mut().insert(
                    HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
        }
    }
    resp
}

async fn host(stub: Stub) -> (String, Arc<Stub>) {
    let stub = Arc::new(stub);
    let app = Router::new().fallback(serve).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}{}",
        listener.local_addr().unwrap().port(),
        stub.prefix.trim_end_matches('/')
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, stub)
}

fn stub(dir: &Path, prefix: &'static str, headers: bool) -> Stub {
    Stub {
        dir: dir.to_path_buf(),
        prefix,
        headers,
        drop_header: None,
        extra: Vec::new(),
        tamper_after: None,
        manifests: AtomicUsize::new(0),
        hits: AtomicUsize::new(0),
    }
}

async fn tor() -> (Transport, FakeSocks) {
    let proxy = FakeSocks::start(Behavior::Passthrough).await;
    (Transport::Socks(proxy.addr.to_string()), proxy)
}

fn plan<'a>(key: &'a str, primary: &'a str, mirror: &'a str) -> Plan<'a> {
    Plan {
        key,
        profile: "fixture",
        primary,
        mirror,
    }
}

// ---- the check ------------------------------------------------------------------------------

#[tokio::test]
async fn both_origins_over_both_routes_agree_with_the_manifest() {
    let site = copy_site();
    let (primary, _) = host(stub(site.path(), "/", true)).await;
    let (mirror, _) = host(stub(site.path(), "/nymira/", false)).await;
    let (transport, proxy) = tor().await;
    let key = text("minisign.pub");
    let report = verify(&plan(&key, &primary, &mirror), &transport, true).await;
    assert!(report.ok(), "{}", report.render());
    assert_eq!(report.runs.len(), 4);
    assert!(report
        .runs
        .iter()
        .all(|r| r.version.as_deref() == Some("0.0.1-fixture")));
    assert!(report.runs[0].headers_checked && !report.runs[1].headers_checked);
    assert!(!proxy.seen().is_empty(), "the Tor leg used the proxy");
    assert!(report.render().contains("cloaking"));
}

#[tokio::test]
async fn a_changed_file_on_one_origin_fails_both_ways() {
    let site = copy_site();
    let bad = copy_site();
    std::fs::write(bad.path().join("assets/app.js"), b"changed").unwrap();
    let (primary, _) = host(stub(site.path(), "/", true)).await;
    let (mirror, _) = host(stub(bad.path(), "/nymira/", false)).await;
    let (transport, _p) = tor().await;
    let key = text("minisign.pub");
    let report = verify(&plan(&key, &primary, &mirror), &transport, true).await;
    assert!(!report.ok());
    assert!(report.runs[1]
        .problems
        .iter()
        .any(|p| p.contains("assets/app.js")));
    assert!(report
        .disagreements
        .iter()
        .any(|d| d.contains("assets/app.js differs between")));
}

#[tokio::test]
async fn a_host_that_answers_differently_the_second_time_is_caught() {
    let site = copy_site();
    let mut s = stub(site.path(), "/", true);
    // The Tor run reads the manifest once; the direct run is the second.
    s.tamper_after = Some(2);
    let (primary, _) = host(s).await;
    let (mirror, _) = host(stub(site.path(), "/nymira/", false)).await;
    let (transport, _p) = tor().await;
    let key = text("minisign.pub");
    let report = verify(&plan(&key, &primary, &mirror), &transport, true).await;
    assert!(!report.ok());
    assert!(
        report
            .disagreements
            .iter()
            .any(|d| d.contains("assets/app.js")),
        "{}",
        report.render()
    );
}

#[tokio::test]
async fn a_missing_signed_header_fails() {
    let site = copy_site();
    let mut s = stub(site.path(), "/", true);
    s.drop_header = Some("content-security-policy");
    let (primary, _) = host(s).await;
    let (mirror, _) = host(stub(site.path(), "/nymira/", false)).await;
    let (transport, _p) = tor().await;
    let key = text("minisign.pub");
    let report = verify(&plan(&key, &primary, &mirror), &transport, true).await;
    assert!(!report.ok());
    assert!(report.runs[0]
        .problems
        .iter()
        .any(|p| p.contains("content-security-policy")));
}

#[tokio::test]
async fn a_bad_signature_a_wrong_profile_and_a_failed_fetch_fail() {
    let site = copy_site();
    let (primary, _) = host(stub(site.path(), "/", true)).await;
    let (transport, _p) = tor().await;
    let key = text("minisign.pub");

    // The second origin is not listening.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
    };
    let report = verify(&plan(&key, &primary, &closed), &transport, true).await;
    assert!(!report.ok());
    assert!(!report.runs[1].problems.is_empty());
    assert!(
        report.runs[0].problems.is_empty(),
        "the other origin is judged alone"
    );

    // The profile in the trusted comment must be the one asked for.
    let (mirror, _) = host(stub(site.path(), "/nymira/", false)).await;
    let mut p = plan(&key, &primary, &mirror);
    p.profile = "mainnet";
    let report = verify(&p, &transport, true).await;
    assert!(!report.ok());
    assert!(report.runs[0].problems[0].contains("trusted comment"));

    // A key that did not sign the manifest.
    let lines: Vec<String> = key.split('\n').map(String::from).collect();
    let mut pk = base64::engine::general_purpose::STANDARD
        .decode(&lines[1])
        .unwrap();
    pk[2] ^= 1;
    let wrong = format!(
        "{}\n{}\n",
        lines[0],
        base64::engine::general_purpose::STANDARD.encode(&pk)
    );
    let report = verify(&plan(&wrong, &primary, &mirror), &transport, true).await;
    assert!(!report.ok());
    assert!(report.runs[0].problems[0].contains("another key"));
}

#[tokio::test]
async fn nothing_goes_out_directly_without_a_confirmation_and_tor_down_is_no_fallback() {
    let site = copy_site();
    let (primary, a) = host(stub(site.path(), "/", true)).await;
    let (mirror, b) = host(stub(site.path(), "/nymira/", false)).await;
    // A proxy that refuses every connection: the Tor leg cannot reach anything.
    let proxy = FakeSocks::start(Behavior::Refuse(5)).await;
    let transport = Transport::Socks(proxy.addr.to_string());
    let key = text("minisign.pub");

    let report = verify(&plan(&key, &primary, &mirror), &transport, false).await;
    assert!(!report.ok());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(
        a.hits.load(Ordering::SeqCst) + b.hits.load(Ordering::SeqCst),
        0
    );

    // With the answer "yes" the direct fetch happens, and the report is still a failure because
    // the Tor leg failed.
    let report = verify(&plan(&key, &primary, &mirror), &transport, true).await;
    assert!(!report.ok());
    assert!(a.hits.load(Ordering::SeqCst) > 0 && b.hits.load(Ordering::SeqCst) > 0);
    assert!(report.runs[2].problems.is_empty() && report.runs[3].problems.is_empty());
}

#[tokio::test]
async fn a_direct_transport_is_not_a_tor_leg() {
    let site = copy_site();
    let (primary, a) = host(stub(site.path(), "/", true)).await;
    let key = text("minisign.pub");
    let report = verify(&plan(&key, &primary, &primary), &Transport::Direct, false).await;
    assert!(!report.ok());
    assert_eq!(a.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn an_empty_pin_refuses_to_run() {
    // The placeholders in this tree are empty, as in a build before the key and the domain exist.
    if MANIFEST_KEY.trim().is_empty() || ORIGINS.trim().is_empty() {
        assert!(compiled().unwrap_err().contains("pins no web manifest key"));
    }
}

// ---- the header Cloudflare Pages adds ---------------------------------------------------------

const ACAO: &str = "access-control-allow-origin";

async fn run_with(extra: Vec<(&'static str, &'static str, &'static str)>) -> Report {
    let site = copy_site();
    let mut s = stub(site.path(), "/", true);
    s.extra = extra;
    let (primary, _) = host(s).await;
    let (mirror, _) = host(stub(site.path(), "/nymira/", false)).await;
    let (transport, _p) = tor().await;
    let key = text("minisign.pub");
    verify(&plan(&key, &primary, &mirror), &transport, true).await
}

#[tokio::test]
async fn acao_star_on_the_documents_is_a_note_and_the_check_succeeds() {
    // The documents are in the `app` class (`/`) and the `frame` class (`/wallet-host*`).
    let report = run_with(vec![("app", ACAO, "*"), ("frame", ACAO, "*")]).await;
    assert!(report.ok(), "{}", report.render());
    let text = report.render();
    // Three paths, on the Tor and the direct run of the primary origin.
    assert_eq!(
        text.lines().filter(|l| l.starts_with("note: http")).count(),
        6,
        "{text}"
    );
    for path in ["/:", "/wallet-host.html:", "/wallet-host:"] {
        assert!(text.contains(&format!("{path} access-control-allow-origin is *")));
    }
    assert!(text.contains("added by Cloudflare Pages"));
    assert!(text.contains("note: 6 header difference(s) accepted"));
    assert!(text.contains("All fetches match the signed manifest and each other."));
    assert!(!text.contains("Do not trust this site"));
    assert!(text.contains("cloaking"), "the limits text stays");
    assert!(report.runs.iter().all(|r| r.problems.is_empty()));
}

#[tokio::test]
async fn another_value_on_a_document_still_fails() {
    for value in ["https://evil.example", "null", "**"] {
        let report = run_with(vec![("app", ACAO, value)]).await;
        assert!(!report.ok(), "{value}");
        assert!(report.runs[0]
            .problems
            .iter()
            .any(|p| p.starts_with("/: access-control-allow-origin")));
        assert!(report.render().contains("Do not trust this site"));
    }
}

#[tokio::test]
async fn a_different_value_on_an_asset_still_fails_where_the_set_signs_star() {
    let report = run_with(vec![("assets", ACAO, "https://evil.example")]).await;
    assert!(!report.ok());
    assert!(report.runs[0]
        .problems
        .iter()
        .any(|p| p.contains("access-control-allow-origin is https://evil.example")));
}

#[tokio::test]
async fn another_signed_header_with_star_on_a_document_still_fails() {
    let report = run_with(vec![("app", "referrer-policy", "*")]).await;
    assert!(!report.ok());
    assert!(report.runs[0]
        .problems
        .iter()
        .any(|p| p.contains("referrer-policy is *")));
}

#[test]
fn only_the_one_difference_on_the_document_paths_is_a_note() {
    use Verdict::*;
    let star = Some("*");
    for path in ["/", "/wallet-host.html", "/wallet-host"] {
        assert!(matches!(
            header_verdict(path, ACAO, star, None),
            Some(Note(n)) if n.contains("Cloudflare Pages")
        ));
    }
    // Equal values say nothing.
    assert_eq!(header_verdict("/", ACAO, star, star), None);
    // Everything else is a problem.
    let problem = |v: Option<Verdict>| matches!(v, Some(Problem(_)));
    assert!(problem(header_verdict("/assets/app.js", ACAO, star, None)));
    assert!(problem(header_verdict("/executors/a.js", ACAO, star, None)));
    assert!(problem(header_verdict("/other.html", ACAO, star, None)));
    assert!(problem(header_verdict(
        "/",
        ACAO,
        Some("https://a.example"),
        None
    )));
    assert!(
        problem(header_verdict("/", ACAO, None, star)),
        "a missing signed header"
    );
    assert!(problem(header_verdict(
        "/",
        ACAO,
        star,
        Some("https://a.example")
    )));
    assert!(problem(header_verdict(
        "/",
        "content-security-policy",
        star,
        None
    )));
    assert!(problem(header_verdict(
        "/",
        "referrer-policy",
        None,
        Some("no-referrer")
    )));
}
