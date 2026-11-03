#!/usr/bin/env bash
# Builds the web bundle twice from the same commit, in two different directories with separate
# cargo output, and fails unless both bundle hashes are equal (web/INTEGRITY.md). The commit is
# HEAD of the checkout this script lives in, taken with `git archive`, so local changes do not count.
#   web/scripts/repro-check.sh [profile...]      profiles: testnet mainnet (default: both)
# Prints one line per profile. About 10 minutes on a cold cache; needs disk for two cargo
# target directories.
set -euo pipefail
profiles=("$@")
[ ${#profiles[@]} -gt 0 ] || profiles=(testnet mainnet)
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

build() { # $1 = directory name; prints "<profile> <hash>" lines
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
    for p in "${profiles[@]}"; do
      node node_modules/vite/bin/vite.js build --mode "$p" --outDir "dist-$p" >&2
      echo "$p $(node scripts/integrity.mjs hash "dist-$p")"
    done
  )
}

first="$(build first)"
second="$(build second-checkout-with-a-longer-name)"
echo "first:"; echo "$first"
echo "second:"; echo "$second"
[ "$first" = "$second" ] || { echo "the two builds differ" >&2; exit 1; }
echo "reproducible"
