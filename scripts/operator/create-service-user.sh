#!/bin/bash
# Creates the dedicated macOS user the operator runs as, its directories and its launchd jobs.
# Run once, as an administrator, on the operator laptop: sudo scripts/operator/create-service-user.sh
# It changes the system (a user, directories under /var/nymira, launchd jobs), so read it first.
set -euo pipefail

USER_NAME="${NYMIRA_SERVICE_USER:-nymira-op}"
HOME_DIR="/var/nymira"
BIN="${NYMIRA_BIN:-/usr/local/libexec/nymira/nymira-operatord}"

if [ "$(id -u)" -ne 0 ]; then echo "run with sudo" >&2; exit 1; fi
if [ ! -x "$BIN" ]; then echo "build and install the binary at $BIN first" >&2; exit 1; fi

if ! id "$USER_NAME" >/dev/null 2>&1; then
  # No password, no login shell, hidden from the login window, not an administrator.
  sysadminctl -addUser "$USER_NAME" -fullName "Nymira operator" -UID 399 \
    -shell /usr/bin/false -home "$HOME_DIR" -roleAccount >/dev/null
  dscl . -create "/Users/$USER_NAME" IsHidden 1
fi

install -d -m 0700 -o "$USER_NAME" "$HOME_DIR" "$HOME_DIR/run" "$HOME_DIR/data" "$HOME_DIR/secrets" "$HOME_DIR/logs"
for role in minter signer closer watcher; do
  install -d -m 0700 -o "$USER_NAME" "$HOME_DIR/secrets/$role"
done

# The processes do not start by themselves: the signer and the minter need passphrases typed at
# start-up, so a person starts them (scripts/operator/start.sh). The jobs are only the watcher,
# the closer and the API, which hold no passphrase, and they wait for the sockets.
echo "user $USER_NAME and $HOME_DIR are ready."
echo "next: put operator.json in $HOME_DIR, make the keys with 'nymira-operatord keygen', then run scripts/operator/start.sh"
