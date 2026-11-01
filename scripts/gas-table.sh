#!/usr/bin/env bash
# Runs the vault's sandbox tests and turns the gas and measurement lines they print into
# markdown tables (default target/gas-table.md). Set NYMIRA_SANDBOX_TOKEN=mainnet to run them
# against the real mainnet USDC wasm. The ignored one-day finalize test is not included.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-target/gas-table.md}
log=$(cargo test -p nymira-vault --test sandbox -- --nocapture --test-threads=4 2>&1 | grep -E '^(gas|measure) ' | sort -u)
{
  echo "| Call | Tgas |"
  echo "|---|---|"
  echo "$log" | sed -nE 's/^gas (.*): ([0-9.]+) Tgas.*/| \1 | \2 |/p'
  echo
  echo "| Measurement | Value |"
  echo "|---|---|"
  echo "$log" | sed -nE 's/^measure ([^:]*): (.*)$/| \1 | \2 |/p'
} > "$out"
echo "wrote $out"
