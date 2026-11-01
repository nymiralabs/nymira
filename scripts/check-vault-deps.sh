#!/usr/bin/env bash
# The vault may depend only on near-sdk, the vendored zkapi-core,
# nymira-near-codec and nymira-verifier.
set -euo pipefail
allowed='near-sdk zkapi-core nymira-near-codec nymira-verifier'
bad=0
for dep in $(cargo tree -p nymira-vault -e normal --depth 1 --prefix none --locked | tail -n +2 | awk '{print $1}' | sort -u); do
  case " $allowed " in
    *" $dep "*) ;;
    *) echo "vault depends on $dep, which is not allowed" >&2; bad=1 ;;
  esac
done
exit $bad
