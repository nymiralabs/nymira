#!/usr/bin/env bash
# Builds the vault in the pinned cargo-near 0.22.0 / rustc 1.99.0 image and
# deploys the result on the sandbox: a build without dev-setup must refuse the
# development keys. Needs Docker and a clean git tree.
set -euo pipefail
cd "$(dirname "$0")/../contracts/vault"
# Colima shares only $HOME with its VM; the build mounts a temp dir.
if [ "$(uname)" = Darwin ]; then export TMPDIR="$HOME/.cache/nymira-tmp"; mkdir -p "$TMPDIR"; fi
cargo near build reproducible-wasm
wasm=$(cd "${CARGO_TARGET_DIR:-../../target}/near/nymira_vault" && pwd)/nymira_vault.wasm
sha=$(shasum -a 256 "$wasm" | cut -d' ' -f1)
echo "reproducible wasm sha256: $sha"
# The test deploys the file given here and checks the on-chain code hash against $sha.
NYMIRA_VAULT_WASM="$wasm" NYMIRA_VAULT_WASM_SHA256="$sha" cargo test --locked --test sandbox a_build_without_dev_setup_refuses_the_development_keys
# Nothing may have rebuilt the file under the test.
[ "$(shasum -a 256 "$wasm" | cut -d' ' -f1)" = "$sha" ] || { echo "wasm changed during the smoke test" >&2; exit 1; }
