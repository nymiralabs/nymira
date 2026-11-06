#!/bin/bash
# Creates the dedicated macOS user the operator runs as and its directories under /var/nymira.
# Run once, as an administrator, on the operator laptop: sudo scripts/operator/create-service-user.sh
# It changes the system (a user and directories under /var/nymira), so read it first.
# A clean PATH: sudo keeps the caller's, which may hold another `install` or `sudo`.
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
set -euo pipefail

USER_NAME="${NYMIRA_SERVICE_USER:-nymira-op}"
HOME_DIR="/var/nymira"
BIN="${NYMIRA_BIN:-/usr/local/libexec/nymira/nymira-operatord}"

if [ "$(id -u)" -ne 0 ]; then echo "run with sudo" >&2; exit 1; fi
if [ ! -x "$BIN" ]; then
  echo "install the binary at $BIN first:" >&2
  echo "  cargo build --profile operator -p nymira-operatord   (add --features dev-setup for rehearsal keys)" >&2
  echo "  sudo /usr/bin/install -d -m 755 $(dirname "$BIN") && sudo /usr/bin/install -m 755 target/operator/nymira-operatord $BIN" >&2
  exit 1
fi

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

# No process starts by itself: the signer and the minter need passphrases typed at start-up, so a
# person starts all five with scripts/operator/start.sh. This script installs no launchd job.
echo "user $USER_NAME and $HOME_DIR are ready."
echo "next: put operator.json in $HOME_DIR, make the keys with 'nymira-operatord keygen', then run scripts/operator/start.sh"
