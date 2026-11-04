#!/usr/bin/env bash
# A release nymirad holds neither the `mock` feature nor a demo key (issue 39). Two checks:
#   1. cargo's feature graph for `nymirad` does not switch `mock` on;
#   2. the binary contains none of the strings that only exist in a demo build.
# With no argument it builds the release binary first.
#   scripts/check-release-binary.sh [path/to/nymirad]
#   scripts/check-release-binary.sh --control     also builds a `mock` binary and requires the
#                                                  string check to reject it, so a pass means
#                                                  something: the check can fail.
set -euo pipefail
cd "$(dirname "$0")/.."

# Present in a `--features mock` build (cfg-gated demo code), absent from a release build:
#   demo-pins.json      the pins file a demo vault writes next to the state
#   demo build:         the demo attestor's messages (it trusts a demo policy signer key)
MARKERS=("demo-pins.json" "demo build:")

target="${CARGO_TARGET_DIR:-target}"

scan() { # $1 = binary; prints the markers it contains
  local bin="$1" found=0
  for m in "${MARKERS[@]}"; do
    if grep -aqF -- "$m" "$bin"; then
      echo "  $bin contains \"$m\"" >&2
      found=1
    fi
  done
  return $found
}

control=0
bin=""
for arg in "$@"; do
  case "$arg" in
    --control) control=1 ;;
    *) bin="$arg" ;;
  esac
done

# `{f}` prints the features each package is built with: `mock` on nymirad itself, `dev-setup` on
# zkapi-proof. (The `-e features` edges also list dev-dependency edges, so a grep on those would
# match every graph, or none.)
graph() { cargo tree -p nymirad --locked -e normal,build "$@" -f '{p} [{f}]'; }
if graph | grep -E '^nymirad v[^[]*\[[^]]*mock' >/dev/null; then
  echo "the release feature graph of nymirad enables \`mock\`" >&2
  exit 1
fi

# The development keys load only with zkapi-proof's `dev-setup`: a release graph must not switch
# it on, or the binary would accept keys nobody ran a ceremony for.
if graph | grep -E 'zkapi-proof v[^[]*\[[^]]*dev-setup' >/dev/null; then
  echo "the release feature graph of nymirad enables zkapi-proof's dev-setup" >&2
  exit 1
fi
# The same greps must see a mock graph, or they check nothing.
if ! graph --features mock | grep -E 'zkapi-proof v[^[]*\[[^]]*dev-setup' >/dev/null \
  || ! graph --features mock | grep -E '^nymirad v[^[]*\[[^]]*mock' >/dev/null; then
  echo "the graph check does not see \`mock\` or \`dev-setup\` in a mock build: it is broken" >&2
  exit 1
fi

if [ -z "$bin" ]; then
  cargo build --release --locked -p nymirad --bin nymirad
  bin="$target/release/nymirad"
fi
scan "$bin" || { echo "$bin is not a release build" >&2; exit 1; }
echo "release nymirad: no mock feature, no demo strings ($(wc -c <"$bin") bytes)"

if [ "$control" = 1 ]; then
  # Build into its own directory so the release binary above stays as it was.
  mock_target="$target/mock-control"
  CARGO_TARGET_DIR="$mock_target" cargo build --release --locked -p nymirad --bin nymirad --features mock
  if scan "$mock_target/release/nymirad" 2>/dev/null; then
    echo "control failed: the string check accepted a mock build" >&2
    exit 1
  fi
  echo "control: the mock build is rejected"
fi
