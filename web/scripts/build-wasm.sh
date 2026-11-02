#!/usr/bin/env bash
# Builds the two WASM modules the app loads into web/src/wasm/ (a build output, not committed):
#   zkapi-browser  the vendored zkapi wallet core: note secret and commitment, genesis state
#   near-codec     nymira-near-codec: the account-to-field mapping
#   web-checks     nymira-web-wasm: the status nonce signature and Intel TDX quote verification
#   zkapi-browser-dev, web-checks-test  test builds (mock profile and Node tests only)
# wasm-bindgen-cli must be 0.2.117: both crates are pinned to that schema version.
set -euo pipefail

CLI_VERSION=0.2.117
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

if ! command -v wasm-bindgen >/dev/null || [ "$(wasm-bindgen --version | awk '{print $2}')" != "$CLI_VERSION" ]; then
  cargo install wasm-bindgen-cli --version "$CLI_VERSION" --locked
fi

# CARGO_TARGET_DIR, when set, holds both workspaces' output.
ZK_TARGET="${CARGO_TARGET_DIR:-$root/vendor/zkapi/protocol/rust/target}"
WS_TARGET="${CARGO_TARGET_DIR:-$root/target}"
out="$root/web/src/wasm"
rm -rf "$out"
mkdir -p "$out"

# zkapi-browser lives in the vendored workspace, which has its own lockfile and target directory.
# The release profile there has no size settings, so they come from the environment.
# `zkapi-browser` is the bundle for the ceremony setup (what a deployed profile loads);
# `zkapi-browser-dev` is built with `dev-setup` and loads the development keys. Only the mock
# profile (Playwright and unit tests) uses the dev build; `check-bundle` refuses it elsewhere.
zk_build() { # $1 = output directory name, rest = extra cargo args
  local name="$1"; shift
  (
    cd vendor/zkapi/protocol/rust
    CARGO_PROFILE_RELEASE_OPT_LEVEL=z CARGO_PROFILE_RELEASE_LTO=true \
      CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_PANIC=abort \
      cargo build --locked --release -p zkapi-browser --target wasm32-unknown-unknown "$@"
  )
  wasm-bindgen --target web --out-dir "$out/$name" \
    "${ZK_TARGET}"/wasm32-unknown-unknown/release/zkapi_browser.wasm
}
zk_build zkapi-browser
zk_build zkapi-browser-dev --features dev-setup

cargo build --locked --release -p nymira-near-codec-wasm --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir "$out/near-codec" \
  "${WS_TARGET}"/wasm32-unknown-unknown/release/nymira_near_codec_wasm.wasm

cargo build --locked --release -p nymira-web-wasm --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir "$out/web-checks" \
  "${WS_TARGET}"/wasm32-unknown-unknown/release/nymira_web_wasm.wasm

# The test operator (state key and signed responses) for the browser tests: never in a bundle.
cargo build --locked --release -p nymira-web-wasm --features test-operator --target wasm32-unknown-unknown
wasm-bindgen --target nodejs --out-dir "$out/web-checks-test" \
  "${WS_TARGET}"/wasm32-unknown-unknown/release/nymira_web_wasm.wasm

echo "wasm built: web/src/wasm"
ls -l "$out"/*/*.wasm
