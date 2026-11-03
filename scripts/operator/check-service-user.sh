#!/bin/bash
# Checks the service user and its files. Changes nothing. Run with sudo.
# usage: sudo scripts/operator/check-service-user.sh [user]
# A clean PATH: sudo keeps the caller's.
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
set -u
USER_NAME="${1:-nymira-op}"
HOME_DIR="/var/nymira"
fail=0
# The service directories are mode 700, so only root can look inside them.
if [ "$(id -u)" -ne 0 ]; then echo "run with sudo: sudo $0 $USER_NAME" >&2; exit 2; fi
check() { if eval "$2"; then echo "ok    $1"; else echo "FAIL  $1"; fail=1; fi; }

check "$HOME_DIR exists" "[ -d $HOME_DIR ]"

check "user $USER_NAME exists" "id $USER_NAME >/dev/null 2>&1"
check "login shell is /usr/bin/false" "[ \"\$(dscl . -read /Users/$USER_NAME UserShell 2>/dev/null | awk '{print \$2}')\" = /usr/bin/false ]"
check "not an administrator" "! dseditgroup -o checkmember -m $USER_NAME admin 2>/dev/null | grep -q '^yes'"
check "not in the developer group" "! dseditgroup -o checkmember -m $USER_NAME _developer 2>/dev/null | grep -q '^yes'"
check "home directory is mode 700" "[ \"\$(stat -f %Lp $HOME_DIR)\" = 700 ]"
check "no secret file is readable by others" "[ -z \"\$(find $HOME_DIR/secrets -type f \\( -perm -g=r -o -perm -o=r -o -perm -u=w \\))\" ]"
check "no sockets with a loose mode" "[ -z \"\$(find $HOME_DIR/run -type s \\( -perm -g=rw -o -perm -o=rw \\))\" ]"
check "no Homebrew, Cargo or Rust files in its home" "[ ! -e $HOME_DIR/.cargo ] && [ ! -e $HOME_DIR/.rustup ] && [ ! -e $HOME_DIR/Library/Application\\ Support/Google ]"
echo
echo "Not checked here: the tools in /usr/bin and the apps in /Applications are readable by every user on macOS. The service user has no shell and no home tools, but system-wide developer tools and browsers stay installed (see THREAT_MODEL.md)."
exit $fail
