#!/usr/bin/env bash
# The vault may depend only on near-sdk, the vendored zkapi-core,
# nymira-near-codec and nymira-verifier.
set -euo pipefail
allowed='near-sdk zkapi-core nymira-near-codec nymira-verifier'
bad=0
# A failing `cargo tree` must fail the check, so it runs on its own line (set -e), not in a loop header.
tree=$(cargo tree -p nymira-vault -e normal,build --depth 1 --prefix none --locked)
deps=$(printf '%s\n' "$tree" | tail -n +2 | awk '{print $1}' | sort -u)
[ -n "$deps" ] || { echo "cargo tree listed no dependencies" >&2; exit 1; }
for dep in $deps; do
  case " $allowed " in
    *" $dep "*) ;;
    *) echo "vault depends on $dep, which is not allowed" >&2; bad=1 ;;
  esac
done
exit $bad
