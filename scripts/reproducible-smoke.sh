#!/usr/bin/env bash
# Builds the vault stub in the pinned cargo-near 0.22.0 / rustc 1.99.0 image
# and deploys the result on the sandbox. Needs Docker and a clean git tree.
set -euo pipefail
cd "$(dirname "$0")/../contracts/vault"
cargo near build reproducible-wasm
wasm=$(ls ../../target/near/nymira_vault/nymira_vault.wasm)
NYMIRA_VAULT_WASM="$(cd "$(dirname "$wasm")" && pwd)/$(basename "$wasm")" cargo test --locked --test sandbox
