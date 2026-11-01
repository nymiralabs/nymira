# Nymira

Private, prepaid API credits on NEAR. Work in progress.

Nymira reuses the zero-knowledge circuits of [zkapi](https://github.com/ethereum/zkapi) (`ethereum/zkapi`), vendored at a pinned commit in `vendor/zkapi/`. `NOTICE` lists the exact commit and every patch we apply. `PINS.md` records the pins and the toolchain.

## Build

The toolchain is pinned in `rust-toolchain.toml` (rustc 1.99.0, `wasm32-unknown-unknown`). Install cargo-near 0.22.0, then:

```bash
cargo check --target wasm32-unknown-unknown --locked -p nymira-vault -p nymira-near-codec -p nymira-verifier -p verifier-probe -p mock-usdc -p mock-ft-receiver -p nymira-near-codec-wasm
cargo test --workspace --locked
```
