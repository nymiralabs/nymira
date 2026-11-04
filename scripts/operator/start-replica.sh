#!/bin/bash
# Starts the second machine's three processes: the replica node, the second watcher and the
# dead-man switch. None of them asks for a passphrase. Run it as the user that owns the data and
# secrets directories of the config, after `keygen replica`, `keygen answer` and `keygen upkeep`.
#   scripts/operator/start-replica.sh /var/lib/nymira/replica.json
set -euo pipefail
CONFIG="${1:?config path}"
BIN="${NYMIRA_BIN:-nymira-operatord}"
LOGS="${NYMIRA_LOGS:-$HOME/nymira-logs}"
mkdir -p "$LOGS"

"$BIN" replica --config "$CONFIG" >>"$LOGS/replica.log" 2>&1 &
"$BIN" second-watcher --config "$CONFIG" >>"$LOGS/second-watcher.log" 2>&1 &
"$BIN" deadman --config "$CONFIG" >>"$LOGS/deadman.log" 2>&1 &
wait
