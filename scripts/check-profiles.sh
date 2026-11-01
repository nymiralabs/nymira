#!/usr/bin/env bash
# Builds that people deposit into must not accept development keys, and the
# mainnet profile must not take the test-only Intents account. Each of these
# feature sets has to fail to compile.
set -u
status=0
for features in mainnet,dev-setup judge,dev-setup mainnet,test-intents-account; do
  if cargo check -p nymira-vault --target wasm32-unknown-unknown --locked --features "$features" >/dev/null 2>&1; then
    echo "build with features [$features] compiled, but must be refused" >&2
    status=1
  fi
done
# Control: a plain mainnet build still compiles, so the failures above come from the guards.
cargo check -p nymira-vault --target wasm32-unknown-unknown --locked --features mainnet || status=1
exit $status
