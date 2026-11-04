#!/usr/bin/env bash
# Builds that people deposit into must not accept development keys, and the
# mainnet profile must not take the test-only Intents account. Each of these
# feature sets has to fail to compile.
set -u
status=0
"$(dirname "$0")/check-pins.sh" --final || status=1
for features in mainnet,dev-setup judge,dev-setup mainnet,test-intents-account; do
  out=$(cargo check -p nymira-vault --target wasm32-unknown-unknown --locked --features "$features" 2>&1) && {
    echo "build with features [$features] compiled, but must be refused" >&2
    status=1
    continue
  }
  # It must fail on our guard, not on something unrelated.
  echo "$out" | grep -q "must not enable" || {
    echo "build with features [$features] failed, but not on the profile guard:" >&2
    echo "$out" >&2
    status=1
  }
done
# Control: a plain mainnet build still compiles, so the failures above come from the guards.
cargo check -p nymira-vault --target wasm32-unknown-unknown --locked --features mainnet || status=1
exit $status
