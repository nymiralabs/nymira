#!/usr/bin/env bash
# Builds the vault in the pinned cargo-near 0.22.0 / rustc 1.99.0 image and
# deploys the result on the sandbox: a build without dev-setup must refuse the
# development keys. Builds twice and compares. Needs Docker and a clean git tree (a plain clone, not a worktree).
set -euo pipefail
cd "$(dirname "$0")/../contracts/vault"
# Colima shares only $HOME with its VM; the build mounts a temp dir.
if [ "$(uname)" = Darwin ]; then export TMPDIR="$HOME/.cache/nymira-tmp"; mkdir -p "$TMPDIR"; fi
# Two builds of one commit must give one file: the contract embeds its source revision, so only
# the same commit can match. A published code hash names the commit it was built from.
build() {
  cargo near build reproducible-wasm
  local file
  file=$(cd "${CARGO_TARGET_DIR:-../../target}/near/nymira_vault" && pwd)/nymira_vault.wasm
  shasum -a 256 "$file" | cut -d' ' -f1
}
wasm=$(cd "${CARGO_TARGET_DIR:-../../target}/near/nymira_vault" 2>/dev/null && pwd || true)/nymira_vault.wasm
first=$(build)
rm -f "$wasm"
second=$(build)
echo "commit $(git rev-parse HEAD)"
echo "first build sha256:  $first"
echo "second build sha256: $second"
[ "$first" = "$second" ] || { echo "two builds of one commit differ" >&2; exit 1; }
sha=$second
# The test deploys the file given here and checks the on-chain code hash against $sha.
NYMIRA_VAULT_WASM="$wasm" NYMIRA_VAULT_WASM_SHA256="$sha" cargo test --locked --test sandbox a_build_without_dev_setup_refuses_the_development_keys
# Nothing may have rebuilt the file under the test.
[ "$(shasum -a 256 "$wasm" | cut -d' ' -f1)" = "$sha" ] || { echo "wasm changed during the smoke test" >&2; exit 1; }
