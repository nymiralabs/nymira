#!/usr/bin/env bash
# One-time OS-user check for agent sockets and token files (macOS, needs sudo).
#
# CI cannot create a second OS user, so these checks run once on the owner's machine:
#   (a) the 0640 token file is readable by an agent user in the agent group and by no other user
#   (b) another local user cannot open the agent socket
#   (c) the owner's own user ID is refused on the socket, even with the group's file access
#   (d) the agent user cannot write the counters file or the state directory
# It also checks that the agent user cannot read the owner token, and that the agent user can
# call nymirad over loopback TCP with a `tcp` token (served), while an Origin header, a
# cross-site fetch and a wrong Host are refused, the `unix` token is refused on TCP, the `tcp`
# token is refused on the socket and the owner token is refused on the socket (e).
#
# It needs a mock-mode build and a running `make demo` stack (the demo prints its state
# directory; the funding key and passphrase file sit in it):
#   cargo build -p nymirad --features mock
#   make demo                                   # in another terminal
#   scripts/agent-os-user-check.sh --home DEMO_STATE_DIR
#
# It creates a throwaway user and group (nymtest_agent, nymtest_agents), registers two budget
# notes with `agent create` (a `unix` and a `tcp` agent; you type `yes` twice), starts `serve` on a spare
# port, runs the checks, and removes the user, the group, the socket directory and the
# temporary files, also when a check or step fails. The demo's registry keeps the agent entry
# and the demo's note stays funded: use a fresh `make demo` afterwards. No token is printed.

set -u -o pipefail

HOME_DIR=""
NYMIRAD="target/debug/nymirad"
PORT=18788
U=nymtest_agent
G=nymtest_agents
AGENT=oscheck
TCP_AGENT=oscheck-tcp

while [ $# -gt 0 ]; do
  case "$1" in
    --home) HOME_DIR="${2:-}"; shift 2 ;;
    --nymirad) NYMIRAD="${2:-}"; shift 2 ;;
    --port) PORT="${2:-}"; shift 2 ;;
    *) echo "usage: $0 --home DEMO_STATE_DIR [--nymirad PATH] [--port PORT]" >&2; exit 2 ;;
  esac
done
die() { echo "error: $*" >&2; exit 2; }
[ -n "$HOME_DIR" ] || die "--home DEMO_STATE_DIR is required"
[ "$(uname)" = Darwin ] || die "this script uses dscl and runs on macOS only"
[ "$(id -u)" -ne 0 ] || die "run it as the owner, not as root; it calls sudo itself"
[ -x "$NYMIRAD" ] || die "$NYMIRAD is not built: cargo build -p nymirad --features mock"
[ -f "$HOME_DIR/funding.key" ] && [ -f "$HOME_DIR/passphrase" ] || die "$HOME_DIR is not a make demo state directory"
dscl . -read "/Users/$U" >/dev/null 2>&1 && die "user $U exists already; remove it first"
dscl . -read "/Groups/$G" >/dev/null 2>&1 && die "group $G exists already; remove it first"
SOCKDIR="/tmp/nymira-agent-$(id -u)"
[ ! -e "$SOCKDIR" ] || die "$SOCKDIR exists; stop the nymirad that uses it and remove it"
sudo -v || die "sudo is needed"

OWNER="$(id -un)"
# Under /tmp, not $TMPDIR: the agent user must be able to walk to the token file.
TMP="$(mktemp -d /tmp/nymtest.XXXXXX)"
chmod 755 "$TMP"
TOKEN="$TMP/agent.token"
TCP_TOKEN="$TMP/agent-tcp.token"
SERVE_PID=""
MADE_USER=0
MADE_GROUP=0
FAILED=0

pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }

cleanup() {
  [ -n "$SERVE_PID" ] && kill "$SERVE_PID" 2>/dev/null && wait "$SERVE_PID" 2>/dev/null
  [ "$MADE_USER" = 1 ] && sudo dscl . -delete "/Users/$U" 2>/dev/null
  [ "$MADE_GROUP" = 1 ] && sudo dscl . -delete "/Groups/$G" 2>/dev/null
  sudo rm -rf "$SOCKDIR" "$TMP"
  echo "cleaned up: user $U, group $G, $SOCKDIR and $TMP are gone"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

free_id() { # first unused ID from 700 up in the given dscl list
  local used n=700
  used="$(dscl . -list "$1" "$2" | awk '{print $2}')"
  while echo "$used" | grep -qx "$n"; do n=$((n + 1)); done
  echo "$n"
}

# ---- the throwaway group and user ----
GID="$(free_id /Groups PrimaryGroupID)"
UID_NEW="$(free_id /Users UniqueID)"
sudo dscl . -create "/Groups/$G" && MADE_GROUP=1
sudo dscl . -create "/Groups/$G" PrimaryGroupID "$GID"
sudo dscl . -create "/Users/$U" && MADE_USER=1
sudo dscl . -create "/Users/$U" UniqueID "$UID_NEW"
sudo dscl . -create "/Users/$U" PrimaryGroupID "$GID"
sudo dscl . -create "/Users/$U" UserShell /usr/bin/false
sudo dscl . -create "/Users/$U" RealName "nymira test agent"
sudo dscl . -create "/Users/$U" NFSHomeDirectory /var/empty
[ "$MADE_USER" = 1 ] && [ "$MADE_GROUP" = 1 ] || die "could not create the user and group"
echo "created user $U (uid $UID_NEW) with primary group $G (gid $GID); the owner $OWNER is not in it"

as_agent() { (cd /tmp && sudo -n -u "$U" "$@"); }
as_other() { (cd /tmp && sudo -n -u nobody "$@"); }
# The owner's user ID, once it is in the agent group (see check c), so the file system lets the
# owner reach the socket and only the peer check can refuse it.
as_owner_in_group() { (cd /tmp && sudo -n -u "$OWNER" "$@"); }

# ---- the agent, the socket directory, the token file ----
mkdir "$SOCKDIR" && sudo chgrp "$G" "$SOCKDIR" && sudo chmod 2750 "$SOCKDIR" || die "socket directory"
echo "Creating the budget note; type yes when nymirad asks."
NYMIRA_MODE=mainnet "$NYMIRAD" --home "$HOME_DIR" --passphrase-file "$HOME_DIR/passphrase" \
  agent create --name "$AGENT" --transport unix --agent-user "$U" --group "$G" \
  --token-file "$TOKEN" --key-file "$HOME_DIR/funding.key" --amount 1000000 || die "agent create failed"
# The owner is not in the group, so nymirad printed this sudo step instead of doing it.
sudo chgrp "$G" "$TOKEN" || die "chgrp of the token file"
echo "Creating the second budget note (tcp token); type yes when nymirad asks."
NYMIRA_MODE=mainnet "$NYMIRAD" --home "$HOME_DIR" --passphrase-file "$HOME_DIR/passphrase" \
  agent create --name "$TCP_AGENT" --transport tcp \
  --token-file "$TCP_TOKEN" --key-file "$HOME_DIR/funding.key" --amount 1000000 || die "agent create (tcp) failed"

NYMIRA_MODE=mainnet "$NYMIRAD" --home "$HOME_DIR" --passphrase-file "$HOME_DIR/passphrase" \
  serve --port "$PORT" >"$TMP/serve.out" 2>"$TMP/serve.err" &
SERVE_PID=$!
SOCK="$SOCKDIR/agent.sock"
for _ in $(seq 1 150); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || { sed 's/nym[ao]_[0-9a-f]*/[token]/g' "$TMP/serve.err" >&2; die "serve did not open $SOCK"; }

# curl over the socket with the token read from a file, never from an argument or the output.
# Prints the HTTP status, or 000 when the connection failed or was dropped.
socket_status() { # <runner> <token file>
  "$1" sh -c '
    { printf "Authorization: Bearer "; cat "$2"; echo; } |
      curl -sS --max-time 10 --unix-socket "$1" -H @- -H "Host: localhost:8788" \
        -o /dev/null -w "%{http_code}" http://localhost:8788/v1/models 2>/dev/null
    true' sh "$SOCK" "$TOKEN"
}

echo
echo "-- (a) the token file --"
mode="$(stat -f %Lp "$TOKEN")"; grp="$(stat -f %Sg "$TOKEN")"; own="$(stat -f %Su "$TOKEN")"
[ "$mode" = 640 ] && [ "$grp" = "$G" ] && [ "$own" = "$OWNER" ] \
  && pass "token file is 0640, owned by $OWNER, group $G" \
  || fail "token file is $mode $own:$grp, expected 640 $OWNER:$G"
as_agent sh -c 'cat "$1" >/dev/null' sh "$TOKEN" 2>/dev/null \
  && pass "the agent user (in group $G) can read the token file" \
  || fail "the agent user cannot read the token file"
as_other sh -c 'cat "$1" >/dev/null' sh "$TOKEN" 2>/dev/null \
  && fail "user nobody (not in the group) can read the token file" \
  || pass "user nobody (not in the group) cannot read the token file"
as_agent sh -c 'cat "$1" >/dev/null' sh "$HOME_DIR/owner.token" 2>/dev/null \
  && fail "the agent user can read the owner token" \
  || pass "the agent user cannot read the owner token"

echo
echo "-- agent over the socket (what the checks below are compared with) --"
code="$(socket_status as_agent)"
case "$code" in
  200|402) pass "the agent user is served on the socket (HTTP $code)" ;;
  *) fail "the agent user got HTTP $code on the socket, expected a served answer" ;;
esac

echo
echo "-- (b) another user cannot open the socket --"
code="$(socket_status as_other)"
[ "$code" = 000 ] \
  && pass "user nobody cannot open the socket (no answer)" \
  || fail "user nobody got HTTP $code from the socket"

echo
echo "-- (c) the owner's own user ID is refused --"
sudo dseditgroup -o edit -a "$OWNER" -t user "$G" >/dev/null 2>&1   # goes away with the group
if as_owner_in_group sh -c 'test -r "$1" && test -w "$1"' sh "$SOCK" 2>/dev/null; then
  code="$(socket_status as_owner_in_group)"
  [ "$code" = 000 ] \
    && pass "the owner's UID reaches the socket file but nymirad drops the connection (no answer)" \
    || fail "the owner's UID got HTTP $code from the socket"
else
  fail "could not give the owner file access to the socket, so the peer check was not reached"
fi

echo
echo "-- (e) the agent user over loopback TCP, and the token boundaries --"
# The owner reads the token files (the tcp and owner tokens are 0600) and hands the header to
# the agent user's curl on stdin, so no token appears in an argument or in the output.
authcfg() { printf 'header = "Authorization: Bearer '; tr -d '\n' <"$1"; printf '"\n'; }
tcp_status() { # <token file> [curl args]
  local f="$1"; shift
  authcfg "$f" | as_agent curl -sS --max-time 10 --config - -o /dev/null -w '%{http_code}' \
    "$@" "http://127.0.0.1:$PORT/v1/models" 2>/dev/null
  true
}
sock_status() { # <token file>
  authcfg "$1" | as_agent curl -sS --max-time 10 --config - --unix-socket "$SOCK" \
    -H "Host: localhost:8788" -o /dev/null -w '%{http_code}' http://localhost:8788/v1/models 2>/dev/null
  true
}
refused() { case "$1" in 4??) return 0 ;; *) return 1 ;; esac; }
HOST_OK=(-H "Host: localhost:8788")
code="$(tcp_status "$TCP_TOKEN" "${HOST_OK[@]}")"
case "$code" in
  200|402) pass "the agent user is served over loopback TCP with the tcp token (HTTP $code)" ;;
  *) fail "the agent user got HTTP $code over loopback TCP with the tcp token" ;;
esac
code="$(tcp_status "$TCP_TOKEN" "${HOST_OK[@]}" -H "Origin: http://evil.example")"
refused "$code" && pass "a request with an Origin header is refused (HTTP $code)" || fail "an Origin header got HTTP $code"
code="$(tcp_status "$TCP_TOKEN" "${HOST_OK[@]}" -H "Sec-Fetch-Site: cross-site")"
refused "$code" && pass "Sec-Fetch-Site: cross-site is refused (HTTP $code)" || fail "a cross-site fetch got HTTP $code"
code="$(tcp_status "$TCP_TOKEN")"   # curl sends Host: 127.0.0.1:PORT
refused "$code" && pass "a wrong Host is refused (HTTP $code)" || fail "a wrong Host got HTTP $code"
code="$(tcp_status "$TOKEN" "${HOST_OK[@]}")"
refused "$code" && pass "the unix token is refused on TCP (HTTP $code)" || fail "the unix token got HTTP $code on TCP"
code="$(sock_status "$TCP_TOKEN")"
refused "$code" && pass "the tcp token is refused on the socket (HTTP $code)" || fail "the tcp token got HTTP $code on the socket"
code="$(sock_status "$HOME_DIR/owner.token")"
refused "$code" && pass "the owner token is refused on the agent socket (HTTP $code)" || fail "the owner token got HTTP $code on the socket"
tmode="$(stat -f %Lp "$TCP_TOKEN")"; town="$(stat -f %Su "$TCP_TOKEN")"
[ "$tmode" = 600 ] && [ "$town" = "$OWNER" ] \
  && pass "the tcp token file is 0600, owned by $OWNER" \
  || fail "the tcp token file is $tmode $town, expected 600 $OWNER"

echo
echo "-- (d) the agent user cannot write the owner's state --"
dmode="$(stat -f %Lp "$HOME_DIR")"
[ "$dmode" = 700 ] && pass "state directory is 0700" || fail "state directory is $dmode, expected 700"
COUNTERS="$HOME_DIR/counters-$AGENT.json"
as_agent sh -c 'echo x >> "$1"' sh "$COUNTERS" 2>/dev/null \
  && fail "the agent user wrote the counters file" \
  || pass "the agent user cannot write the counters file"
as_agent sh -c 'touch "$1/probe"' sh "$HOME_DIR" 2>/dev/null \
  && fail "the agent user created a file in the state directory" \
  || pass "the agent user cannot create a file in the state directory"
as_other sh -c 'touch "$1/probe"' sh "$HOME_DIR" 2>/dev/null \
  && fail "user nobody created a file in the state directory" \
  || pass "user nobody cannot create a file in the state directory"
[ -e "$HOME_DIR/probe" ] && { fail "a probe file exists in the state directory"; sudo rm -f "$HOME_DIR/probe"; }
if [ -f "$COUNTERS" ]; then
  cmode="$(stat -f %Lp "$COUNTERS")"
  [ "$cmode" = 600 ] && pass "the counters file is 0600" || fail "the counters file is $cmode, expected 600"
fi

echo
if [ "$FAILED" = 0 ]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; fi
exit "$FAILED"
