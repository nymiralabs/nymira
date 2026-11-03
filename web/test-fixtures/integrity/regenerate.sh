#!/usr/bin/env bash
# Rebuilds `site/`: the files a host serves, with a manifest signed by a throwaway key made here
# and deleted at the end. Only the public key (`minisign.pub`) and the signature are kept. Needs
# the `minisign` tool (0.11 or later). The throwaway key signs nothing else; it is not the release
# key (see web/INTEGRITY.md for that key's rules).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

minisign -G -W -f -p "$here/minisign.pub" -s "$tmp/throwaway.key" -c "nymira integrity test key (throwaway)" >/dev/null
node "$here/../../scripts/integrity.mjs" assemble \
  --dist "$here/src/bundle" --meta "$here/src/meta" --out "$here/site" --version 0.0.1-fixture --profile fixture
minisign -S -s "$tmp/throwaway.key" -m "$here/site/manifest.json" \
  -t "nymira-web-manifest v1 profile=fixture version=0.0.1-fixture" >/dev/null
node "$here/../../scripts/integrity.mjs" verify --site "$here/site" --pubkey "$here/minisign.pub"
node "$here/../../scripts/integrity.mjs" fingerprint "$here/minisign.pub"
node "$here/../../scripts/integrity.mjs" fixture-summary "$here" > "$here/expected.json"
