#!/bin/bash
# Starts the five operator processes as the service user, in order. The minter, signer and
# closer sockets must exist before the API starts. Passphrases are typed here and go to the
# processes through a pipe on standard input; nothing reads them from the environment.
# usage: sudo scripts/operator/start.sh /var/nymira/operator.json
set -euo pipefail
CONFIG="${1:?config path}"
BIN="${NYMIRA_BIN:-/usr/local/libexec/nymira/nymira-operatord}"
USER_NAME="${NYMIRA_SERVICE_USER:-nymira-op}"
LOGS=/var/nymira/logs
run() { sudo -u "$USER_NAME" -H env -i PATH=/usr/bin:/bin "$BIN" --config "$CONFIG" "$@"; }

read -rsp "seal passphrase (key minter): " SEAL; echo
read -rsp "state key passphrase (signer): " STATE; echo
read -rsp "clearance key passphrase (signer): " CLEAR; echo

printf '%s\n' "$SEAL" | run minter >>"$LOGS/minter.log" 2>&1 &
printf '%s\n%s\n' "$STATE" "$CLEAR" | run signer >>"$LOGS/signer.log" 2>&1 &
unset SEAL STATE CLEAR
run closer >>"$LOGS/closer.log" 2>&1 &
run watcher >>"$LOGS/watcher.log" 2>&1 &
sleep 5
run api >>"$LOGS/api.log" 2>&1 &
wait
