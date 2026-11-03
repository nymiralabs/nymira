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

if cargo tree -p nymirad --locked -e normal,build,features | grep -q 'nymirad feature "mock"'; then
  echo "the release feature graph of nymirad enables \`mock\`" >&2
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
