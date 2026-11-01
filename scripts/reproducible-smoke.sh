#!/usr/bin/env bash
# Builds the vault in the pinned cargo-near 0.22.0 / rustc 1.99.0 image and
# deploys the result on the sandbox: a build without dev-setup must refuse the
# development keys. Needs Docker and a clean git tree.
set -euo pipefail
cd "$(dirname "$0")/../contracts/vault"
# Colima shares only $HOME with its VM; the build mounts a temp dir.
if [ "$(uname)" = Darwin ]; then export TMPDIR="$HOME/.cache/nymira-tmp"; mkdir -p "$TMPDIR"; fi
cargo near build reproducible-wasm
wasm=$(ls ../../target/near/nymira_vault/nymira_vault.wasm)
NYMIRA_VAULT_WASM="$(cd "$(dirname "$wasm")" && pwd)/$(basename "$wasm")" cargo test --locked --test sandbox a_build_without_dev_setup_refuses_the_development_keys
