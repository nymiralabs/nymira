//! Rules about the source itself: what the daemon must never do, checked where it can be.

use std::path::Path;

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

#[test]
fn nymirad_never_asks_an_rpc_whether_its_nullifier_is_used() {
    for (path, text) in sources() {
        assert!(!text.contains("is_nullifier_used"), "{path}");
        assert!(!text.contains("nullifier_used"), "{path}");
    }
}

#[test]
fn secrets_never_come_from_the_environment_or_argv_values() {
    for (path, text) in sources() {
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            if line.contains("env::var(") {
                assert!(
                    line.contains("NYMIRA_MODE") || line.contains("\"HOME\""),
                    "{path}: {line}"
                );
            }
        }
    }
}

#[test]
fn a_release_build_has_no_mock_or_attestation_switch() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let features = manifest
        .split("[features]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    let names: Vec<&str> = features
        .lines()
        .filter(|l| l.contains('=') && !l.trim_start().starts_with('#'))
        .map(|l| l.split('=').next().unwrap().trim())
        .collect();
    assert_eq!(
        names,
        ["mock"],
        "no default features, no attestation skipping"
    );
    for (path, text) in sources() {
        assert!(!text.to_lowercase().contains("skip_attestation"), "{path}");
    }
}
