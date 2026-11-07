# Pins

What this repository is built from, with the checks behind each choice. Re-checked on 2026-11-01.

## zkapi

- Upstream: `ethereum/zkapi`, vendored in `vendor/zkapi/`.
- Pin: `4ac25c811318d96a873b8b232c7be2252e16c0bd`, the merge commit of zkapi PR #31 (and #30) on `main`, 2026-10-22.
- Freeze tag: none. zkapi has published no `circuit-freeze-v2c` tag (its tags are `clientd-v0.1.0` to `clientd-v0.1.6`). Nymira pins the commit, not a tag.
- `main` had moved to `cfea634` by 2026-10-29. Everything after the pin touches tickets, the companion and the client wallet. In `protocol/rust` and `protocol/setup/v2-concession-dev` the only differences from the pin are 86 added lines in `zkapi-client/src/wallet.rs` and `zkapi-types/src/wire.rs` (ticket billing). The circuit and the setup keys are unchanged, so the pin was not moved.
- Circuit: `zkapi-v2-concession-v2`.
- Circuit layout: leaf-bound. The withdrawal proof carries `noteLeaf` and `noteFields` (`note_id*2^192 + deposit*2^64 + expiry`) and no Merkle path; the vault compares the leaf, deposit and expiry with what it stored at deposit. The root-based layout of the earlier pin (`activeRoot`, `noteId`) does not ship.
- Vendored paths: `protocol/rust` (five crates) and `protocol/setup/v2-concession-dev`. The first vendor commit is a pure snapshot. The patches follow as separate commits, listed in `NOTICE`.

## Toolchain

- rustc 1.99.0 and the `wasm32-unknown-unknown` target, in `rust-toolchain.toml`, for the whole workspace.
- Rust 1.99.1 exists (2026-10-15). It fixes a `wcslen` link error, an x86_64 FFI `bool` miscompile and a macOS `rust-lld` rpath problem, none of them on wasm. The workspace stays on 1.99.0 because zkapi's toolchain file and browser build script pin 1.99.0. On 2026-11-01 zkapi `main` still pins 1.99.0.
- `near-sdk = "=5.29.1"` with default features off (the default `wee_alloc` is unmaintained). arkworks stays on 0.5.x, which zkapi uses. `near-workspaces = "=0.23.0"`. cargo-near 0.22.0.
- Dependency builds use `opt-level = 3` in the dev profile.
- The vault built with rustc 1.99.0 through cargo-near deploys and runs in the near-workspaces sandbox (`contracts/vault/tests/sandbox.rs`). The reproducible build also works (it builds and deploys; the contract embeds its source revision, so a code hash is tied to the commit, and two builds of one commit on one arm64 Mac were equal, and a build on another CPU architecture was not yet compared): `scripts/reproducible-smoke.sh` builds the vault in `sourcescan/cargo-near:0.22.0-rust-1.99.0` (digest pinned in `contracts/vault/Cargo.toml`) and deploys the result on the sandbox. The 1.96 fallback is not needed, so the vault and the rest of the workspace use one compiler.

## NEAR AI SDK

`nearai-inference-sdk` stays at `=0.1.0`, the version the vendor patch targets. The ticket expected only an RC of 0.2.0; stable 0.2.0 (2026-10-27) and 0.2.1 (2026-10-29) are out. Both still call `.no_proxy()` in `cloud_api.rs`, `nvidia.rs` and `provenance.rs`, so the Tor patch still applies. Diff 0.2.1 against 0.1.0 before moving the pin.

## NEAR Intents

- `intents.near` on mainnet has code hash `BtA1BEFNS619KkvXFsgcpQ21Tb5yMm32aaUkn7xNothk` (view call at block 222557547; the wasm is 1,297,551 bytes, SHA-256 `a1afab98916d9f213746544dcf8e4e6bdb22c0be6bc5dfbd8ea8814c9922b547`). It matches the value last seen, so nothing changed since.
- The sandbox fixture is the open-source Verifier v0.4.2 (`near/intents`, tag `defuse/v0.4.2`, MIT): `crates/nymira-testkit/fixtures/verifier-v0.4.2.wasm`, SHA-256 `f67f9559b3b512c52dfa11723a28b9119bcf773ac01780370d6409e5f1fb20e5`, equal to the release's published digest. It differs from the deployed `intents.near` code. Text says "open-source Verifier v0.4.2", never "the mainnet contract".

Newer stable releases exist: `near-sdk` 5.30.0 (2026-10-27) and `near-workspaces` 0.24.0 (2026-10-26). The pins stay because gas, the sandbox runs and the reproducible image were measured on `near-sdk` 5.29.1 and `near-workspaces` 0.23.0; moving means measuring again.

`judge` is only a marker feature for the profile guard. The shipped vault uses no feature; the sandbox test `a_build_without_dev_setup_refuses_the_development_keys` shows that it refuses the development keys, and `scripts/check-pins.sh` checks that its pins equal the vendored ceremony hashes.

## USDC

The sandbox parity fixture copies the mainnet USDC contract (`17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1`, code hash `DVJx7F3dHEHJYwpYPwXHu8K9Uo7h4kXFcALucY5oNX8q`) from mainnet RPC on use and checks that hash. Circle's testnet USDC (`3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af`) has code hash `EoAUFWELjD9gYzsjKDXYRmHDHf1RJ95jaKNSYE6v9b2z`, so a pass on testnet alone does not show mainnet behaviour. Sandboxes run nearcore 2.13.4 (mainnet) or 2.14.0-rc.5 (testnet); see `crates/nymira-testkit/src/chain.rs`.

## Checks in this repository

- `scripts/check-vault-deps.sh`: the vault depends only on `near-sdk`, `zkapi-core`, `nymira-near-codec` and `nymira-verifier`.
- `scripts/check-profiles.sh`: a mainnet build with `dev-setup`, a `judge` build (public testnet bundle and judge vault) with `dev-setup`, and a mainnet build with `test-intents-account` must fail to compile.
- `cargo audit` and `cargo deny` run in CI. The ignored advisories are in host-side test tooling, in compile-time macro crates, in `nymirad`'s attestation verification (`rsa`, Marvin attack, no fix; used to verify signatures only) and in one crate that is also in the vault's wasm dependency tree (`tracing-subscriber` 0.2.25 through `ark-relations`; the advisory concerns formatted log output, and a contract installs no log subscriber). Each has its reason in `.cargo/audit.toml` and `deny.toml`.
- `nymirad print-pins --network testnet --vault ACCOUNT > crates/nymirad/pins/testnet.json` reads the vault over public RPC and writes the pin file (code hash, `vk_hashes`, pinned `config()` fields, access keys). Review the output against the deployment record before committing it. The proving-key hashes live in `crates/nymirad/pins/proving-key.sha256` and `withdrawal-key.sha256`.
