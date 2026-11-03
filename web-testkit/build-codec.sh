#!/usr/bin/env bash
# Builds the browser NEAR codec into web-testkit/codec-pkg/ (an ES module plus
# its .wasm, usable in the browser and in Node).
set -euo pipefail

CLI_VERSION=0.2.117
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# The wasm-bindgen crate is pinned to =0.2.117 and the CLI refuses any other
# schema version, so check the lockfile and the installed CLI agree.
locked="$(awk '/^name = "wasm-bindgen"$/ {getline; gsub(/[^0-9.]/, ""); print; exit}' Cargo.lock)"
if [ "$locked" != "$CLI_VERSION" ]; then
  echo "Cargo.lock has wasm-bindgen $locked, expected $CLI_VERSION" >&2
  exit 1
fi
if ! command -v wasm-bindgen >/dev/null || [ "$(wasm-bindgen --version | awk '{print $2}')" != "$CLI_VERSION" ]; then
  cargo install wasm-bindgen-cli --version "$CLI_VERSION" --locked
fi

cargo build --locked --release -p nymira-near-codec-wasm --target wasm32-unknown-unknown
rm -rf web-testkit/codec-pkg
wasm-bindgen --target web --out-dir web-testkit/codec-pkg \
  "${CARGO_TARGET_DIR:-target}"/wasm32-unknown-unknown/release/nymira_near_codec_wasm.wasm
echo "codec built: web-testkit/codec-pkg"
