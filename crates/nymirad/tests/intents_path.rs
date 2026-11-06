//! The build-time Intents path (issue 50): the mainnet profile carries one path or none, and the
//! testnet profile ignores the setting.
use nymirad::{
    config::Config,
    mode::{IntentsPath, Mode, MAINNET_INTENTS_PATH},
};
use std::process::{Command, Stdio};

#[test]
fn mainnet_carries_the_compiled_path_and_testnet_is_quote_only() {
    assert_eq!(MAINNET_INTENTS_PATH, IntentsPath::TwoStep);
    assert_eq!(Mode::Mainnet.intents_path(), IntentsPath::TwoStep);
    assert_eq!(Mode::Testnet.intents_path(), IntentsPath::Off);
}

#[test]
fn the_config_cannot_move_testnet_off_quote_only() {
    for path in [IntentsPath::Direct, IntentsPath::TwoStep, IntentsPath::Off] {
        let cfg = Config {
            intents_path: Some(path),
            ..Default::default()
        };
        assert_eq!(cfg.intents_path(Mode::Testnet), IntentsPath::Off);
        assert_eq!(cfg.intents_path(Mode::Mainnet), path);
    }
    assert_eq!(
        Config::default().intents_path(Mode::Mainnet),
        MAINNET_INTENTS_PATH
    );
}

fn deposit(mode: &str, path: IntentsPath) -> std::process::Output {
    let home = tempfile::tempdir().unwrap();
    Config {
        intents_path: Some(path),
        ..Default::default()
    }
    .save(home.path())
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_nymirad"))
        .env("NYMIRA_MODE", mode)
        .arg("--home")
        .arg(home.path())
        .args(["deposit", "--source", "list", "--amount", "1000000"])
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_none_build_refuses_every_intents_command_and_testnet_refuses_whatever_the_config_says() {
    let none = deposit("mainnet", IntentsPath::Off);
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("no Intents funding path"));
    let testnet = deposit("testnet", IntentsPath::Direct);
    assert!(!testnet.status.success());
    assert!(String::from_utf8_lossy(&testnet.stderr).contains("Intents has no testnet"));
}
