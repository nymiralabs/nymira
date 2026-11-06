#!/usr/bin/env bash
# Checks the SSH forward recipes for agents on another machine and the restricted
# `authorized_keys` options, once.
#
#   scripts/agent-ssh-forward-check.sh            # a throwaway sshd as you, on loopback (no sudo)
#   scripts/agent-ssh-forward-check.sh SSH_HOST   # an ssh host that takes your normal key
#
# Without an argument the script starts its own `sshd` on a high loopback port with a temporary
# host key, a temporary client key and an `authorized_keys` file in a temporary directory. It
# touches nothing else and removes everything when it ends, also on failure. With SSH_HOST it
# adds two marked lines to ~/.ssh/authorized_keys there and removes them again. Nothing here
# talks to nymirad: a small local web server stands in for it, serving one word.
#
# Two throwaway keys are installed:
#   key L  restrict,port-forwarding,permitopen="localhost:STUB",command="/bin/false"
#          the README's agent-host line: it may run `ssh -N -L ...:localhost:STUB`
#   key R  restrict,port-forwarding,permitlisten="localhost:RPORT",command="/bin/false"
#          the line for a key that runs `ssh -N -R RPORT:localhost:STUB` against the agent host
#
# Checks (PASS/FAIL each):
#   1. neither key can run a command
#   2. key L: -L to the stub port works
#   3. key L: -L to another port is refused
#   4. key R: -R of the permitted listen port works and the stub answers on it
#   5. key R: -R of another listen port is refused
# INFO lines say what key L (the README options alone) allows for -R.
#
# On one machine the stub and the forwarded port cannot share a number, so the script uses
# STUB (18788) for the stub and RPORT (18790) for the -R listen port; on real machines both are
# 8788. Set SSHD_PORT, STUB, RPORT, LPORT in the environment to change them.

set -u -o pipefail

HOST="${1:-}"
STUB="${STUB:-18788}"
RPORT="${RPORT:-18790}"
LPORT="${LPORT:-28788}"
OTHER="${OTHER:-$((STUB + 1))}"
MARK="nymira-ssh-forward-check"

die() { echo "error: $*" >&2; exit 2; }
for tool in ssh ssh-keygen curl python3; do command -v "$tool" >/dev/null || die "$tool is needed"; done

TMP="$(mktemp -d /tmp/nymssh.XXXXXX)"
PIDS=()
INSTALLED=0
FAILED=0
LOCAL=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

if [ -z "$HOST" ]; then
  LOCAL=1
  HOST=127.0.0.1
  SSH_PORT="${SSHD_PORT:-$(free_port)}"
else
  SSH_PORT="${SSH_PORT:-22}"
fi

KEYL="$TMP/keyL"
KEYR="$TMP/keyR"
SSH_BASE=(-p "$SSH_PORT" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10)
as_me() { ssh "${SSH_BASE[@]}" "$HOST" "$@"; }
with_key() { local k="$1"; shift; ssh "${SSH_BASE[@]}" -o IdentitiesOnly=yes -o IdentityAgent=none -i "$k" "$@"; }
# Runs a shell command on the host side of the forward.
on_host() { if [ "$LOCAL" = 1 ]; then sh -c "$1"; else as_me "$1"; fi; }

cleanup() {
  for pid in "${PIDS[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null; done
  # Background ssh clients are children of subshells: find them by the temp directory in their arguments.
  pkill -f "$TMP" 2>/dev/null
  if [ "$INSTALLED" = 1 ] && [ "$LOCAL" = 0 ]; then
    if as_me "grep -vF '$MARK' ~/.ssh/authorized_keys > ~/.ssh/authorized_keys.$MARK && cat ~/.ssh/authorized_keys.$MARK > ~/.ssh/authorized_keys; rm -f ~/.ssh/authorized_keys.$MARK"; then
      echo "removed the test keys from $HOST"
    else
      echo "WARNING: could not remove the lines marked $MARK from ~/.ssh/authorized_keys on $HOST; remove them by hand" >&2
    fi
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

ssh-keygen -q -t ed25519 -N "" -C "$MARK" -f "$KEYL" || die "ssh-keygen"
ssh-keygen -q -t ed25519 -N "" -C "$MARK" -f "$KEYR" || die "ssh-keygen"
LINE_L="restrict,port-forwarding,permitopen=\"localhost:$STUB\",command=\"/bin/false\" $(cat "$KEYL.pub")"
LINE_R="restrict,port-forwarding,permitlisten=\"localhost:$RPORT\",command=\"/bin/false\" $(cat "$KEYR.pub")"

if [ "$LOCAL" = 1 ]; then
  SSHD=/usr/sbin/sshd
  [ -x "$SSHD" ] || SSHD="$(command -v sshd)" || die "sshd is needed"
  ssh-keygen -q -t ed25519 -N "" -f "$TMP/hostkey" || die "ssh-keygen"
  printf '%s\n%s\n' "$LINE_L" "$LINE_R" >"$TMP/authorized_keys"
  chmod 600 "$TMP/authorized_keys"
  cat >"$TMP/sshd_config" <<EOF
Port $SSH_PORT
ListenAddress 127.0.0.1
HostKey $TMP/hostkey
PidFile $TMP/sshd.pid
AuthorizedKeysFile $TMP/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
LogLevel ERROR
EOF
  "$SSHD" -f "$TMP/sshd_config" -E "$TMP/sshd.log" || die "sshd did not start: $(cat "$TMP/sshd.log" 2>/dev/null)"
  for _ in $(seq 1 50); do [ -s "$TMP/sshd.pid" ] && break; sleep 0.1; done
  [ -s "$TMP/sshd.pid" ] && PIDS+=("$(cat "$TMP/sshd.pid")")
  echo "started a throwaway sshd on 127.0.0.1:$SSH_PORT ($(ssh -V 2>&1))"
else
  as_me true >/dev/null 2>&1 || die "cannot ssh to $HOST with your normal key (BatchMode)"
  as_me "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && printf '%s\n' '$LINE_L' '$LINE_R' >> ~/.ssh/authorized_keys" \
    || die "could not add the test keys to $HOST"
  INSTALLED=1
fi
echo "key L: restrict,port-forwarding,permitopen=\"localhost:$STUB\",command=\"/bin/false\""
echo "key R: restrict,port-forwarding,permitlisten=\"localhost:$RPORT\",command=\"/bin/false\""

# Stand-ins for nymirad: STUB answers "agent-ok", OTHER answers "other-port".
mkdir "$TMP/a" "$TMP/b"
echo agent-ok >"$TMP/a/index.html"
echo other-port >"$TMP/b/index.html"
python3 -m http.server "$STUB" --bind 127.0.0.1 --directory "$TMP/a" >/dev/null 2>&1 &
PIDS+=("$!")
python3 -m http.server "$OTHER" --bind 127.0.0.1 --directory "$TMP/b" >/dev/null 2>&1 &
PIDS+=("$!")
for _ in $(seq 1 50); do
  curl -s "http://127.0.0.1:$STUB/" >/dev/null && curl -s "http://127.0.0.1:$OTHER/" >/dev/null && break
  sleep 0.1
done

wait_for() { # <command...>: retry for ten seconds
  for _ in $(seq 1 50); do "$@" >/dev/null 2>&1 && return 0; sleep 0.2; done
  return 1
}
asks() { curl -fs --max-time 3 "$1" | grep -q "$2"; }

echo
echo "-- 1. the restricted keys open no shell --"
for k in L R; do
  key="$TMP/key$k"
  out="$(with_key "$key" "$HOST" id 2>&1)"; rc=$?
  if [ "$rc" -ne 0 ] && ! echo "$out" | grep -q 'uid='; then
    pass "key $k: ssh $HOST id fails (exit $rc) and prints no uid"
  else
    fail "key $k: ssh $HOST id printed a uid or succeeded (exit $rc): $out"
  fi
done

echo
echo "-- 2. key L: ssh -N -L $LPORT:localhost:$STUB --"
with_key "$KEYL" -N -o ExitOnForwardFailure=yes -L "127.0.0.1:$LPORT:localhost:$STUB" "$HOST" &
PIDS+=("$!")
if wait_for asks "http://127.0.0.1:$LPORT/" agent-ok; then
  pass "the -L forward reaches the stub on port $STUB"
else
  fail "the -L forward does not reach the stub on port $STUB"
fi

echo
echo "-- 3. key L: only port $STUB forwards with -L --"
with_key "$KEYL" -N -L "127.0.0.1:$((LPORT + 1)):localhost:$OTHER" "$HOST" 2>/dev/null &
PIDS+=("$!")
sleep 2
if asks "http://127.0.0.1:$((LPORT + 1))/" other-port; then
  fail "a -L forward to port $OTHER got through (the stub on $OTHER is running)"
else
  pass "a -L forward to port $OTHER is refused (the stub on $OTHER is running)"
fi

echo
echo "-- 4. key R: ssh -N -R $RPORT:localhost:$STUB --"
with_key "$KEYR" -N -o ExitOnForwardFailure=yes -R "$RPORT:localhost:$STUB" "$HOST" &
PIDS+=("$!")
if wait_for on_host "curl -fs --max-time 3 http://127.0.0.1:$RPORT/ | grep -q agent-ok"; then
  pass "the stub answers on the loopback of the host at port $RPORT through the -R forward"
else
  fail "nothing answered on port $RPORT of the host through the -R forward"
fi

echo
echo "-- 5. key R: only listen port $RPORT forwards with -R --"
with_key "$KEYR" -N -o ExitOnForwardFailure=yes -R "$((RPORT + 1)):localhost:$STUB" "$HOST" 2>/dev/null &
EXTRA=$!
PIDS+=("$EXTRA")
sleep 2
if on_host "curl -fs --max-time 3 http://127.0.0.1:$((RPORT + 1))/ | grep -q agent-ok"; then
  fail "a -R forward of port $((RPORT + 1)) got through"
else
  pass "a -R forward of port $((RPORT + 1)) is refused"
fi
kill "$EXTRA" 2>/dev/null

echo
echo "-- INFO: key L alone (the README options, no permitlisten) --"
with_key "$KEYL" -N -o ExitOnForwardFailure=yes -R "$((RPORT + 2)):localhost:$STUB" "$HOST" 2>/dev/null &
EXTRA=$!
PIDS+=("$EXTRA")
sleep 2
if on_host "curl -fs --max-time 3 http://127.0.0.1:$((RPORT + 2))/ | grep -q agent-ok"; then
  echo "INFO  key L may -R forward any port: permitopen limits -L only, permitlisten limits -R"
else
  echo "INFO  key L may not -R forward other ports"
fi
kill "$EXTRA" 2>/dev/null

echo
if [ "$FAILED" = 0 ]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; fi
exit "$FAILED"
