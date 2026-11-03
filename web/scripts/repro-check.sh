#!/usr/bin/env bash
# Builds the web bundle twice from the same commit, in two different directories with separate
# cargo output, and fails unless both bundle hashes are equal (web/INTEGRITY.md). The commit is
# HEAD of the checkout this script lives in, taken with `git archive`, so local changes do not count.
#   web/scripts/repro-check.sh [profile]      profile: testnet (default) or mainnet
# Prints the hash. About 10 minutes on a cold cache; needs disk for two cargo target directories.
set -euo pipefail
profile="${1:-testnet}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

build() { # $1 = directory name
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "$root" archive HEAD | tar -x -C "$dir"
  (
    # Cargo output stays inside each copy, so nothing is shared between the two builds.
    unset CARGO_TARGET_DIR
    cd "$dir"
    web/scripts/build-wasm.sh >&2
    cd web
    npm ci --ignore-scripts >&2
    node node_modules/vite/bin/vite.js build --mode "$profile" --outDir "dist-$profile" >&2
    node scripts/integrity.mjs hash "dist-$profile"
  )
}

first="$(build first)"
second="$(build second-checkout-with-a-longer-name)"
echo "first  $first"
echo "second $second"
[ "$first" = "$second" ] || { echo "the two builds differ" >&2; exit 1; }
echo "reproducible: $first"
