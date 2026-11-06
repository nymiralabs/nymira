#!/usr/bin/env bash
# Checks the SSH forward recipe for agents on another machine, once, against an ssh host.
#
#   scripts/agent-ssh-forward-check.sh [SSH_HOST]      # default: localhost (Remote Login on)
#
# SSH_HOST must accept your normal key without a prompt (BatchMode). The script puts a
# throwaway key on it with the restricted options from the README, marked with
# `nymira-ssh-forward-check`, and removes the line again when it ends, also on failure. Use a
# machine you may edit ~/.ssh/authorized_keys on. Nothing here talks to nymirad: a small local
# web server stands in for it, serving one word.
#
# Checks (PASS/FAIL each):
#   1. the restricted key cannot run a command (`id` and `echo` produce no output, ssh fails)
#   2. `ssh -N -R PORT:localhost:PORT HOST`, the README recipe started on your machine, makes
#      the stand-in answer on the loopback of HOST
#   3. `ssh -N -L PORT:localhost:PORT HOST`, the variant started on the agent host, works
#   4. with that key, a -L forward to any other port on HOST is refused (permitopen)
# One more line is INFO: whether the same key may -R forward other ports. The README options
# restrict the direction -L only; `permitlisten="localhost:PORT"` limits -R.
#
# On the same machine the forwarded port cannot equal the stand-in's, so with the default
# localhost the -R side binds REMOTE_PORT (18788) instead of 8788. Set PORT, REMOTE_PORT,
# SSH_PORT in the environment to change the ports.

set -u -o pipefail

HOST="${1:-localhost}"
PORT="${PORT:-8788}"
SSH_PORT="${SSH_PORT:-22}"
case "$HOST" in
  localhost|127.0.0.1|::1) REMOTE_PORT="${REMOTE_PORT:-18788}" ;;
  *) REMOTE_PORT="${REMOTE_PORT:-$PORT}" ;;
esac
LPORT="${LPORT:-28788}"     # local end of the -L forward
OTHER="${OTHER:-$((PORT + 1))}"
MARK="nymira-ssh-forward-check"
OPTS="restrict,port-forwarding,permitopen=\"localhost:$PORT\",command=\"/bin/false\""

die() { echo "error: $*" >&2; exit 2; }
for tool in ssh ssh-keygen curl python3; do command -v "$tool" >/dev/null || die "$tool is needed"; done

TMP="$(mktemp -d)"
PIDS=()
INSTALLED=0
FAILED=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }

SSH_BASE=(-p "$SSH_PORT" -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10)
as_me() { ssh "${SSH_BASE[@]}" "$HOST" "$@"; }
KEY="$TMP/key"
as_key() { ssh "${SSH_BASE[@]}" -o IdentitiesOnly=yes -o IdentityAgent=none -i "$KEY" "$@"; }

cleanup() {
  for pid in "${PIDS[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null; done
  if [ "$INSTALLED" = 1 ]; then
    if as_me "grep -vF '$MARK' ~/.ssh/authorized_keys > ~/.ssh/authorized_keys.$MARK && cat ~/.ssh/authorized_keys.$MARK > ~/.ssh/authorized_keys; rm -f ~/.ssh/authorized_keys.$MARK"; then
      echo "removed the test key from $HOST"
    else
      echo "WARNING: could not remove the line marked $MARK from ~/.ssh/authorized_keys on $HOST; remove it by hand" >&2
    fi
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

as_me true >/dev/null 2>&1 || die "cannot ssh to $HOST with your normal key (BatchMode)"
ssh-keygen -q -t ed25519 -N "" -C "$MARK" -f "$KEY" || die "ssh-keygen"
as_me "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && printf '%s\n' '$OPTS $(cat "$KEY.pub")' >> ~/.ssh/authorized_keys" \
  || die "could not add the test key to $HOST"
INSTALLED=1
echo "installed a test key on $HOST with: $OPTS"

# Stand-ins for nymirad: PORT answers "agent-ok", OTHER answers "other-port".
mkdir "$TMP/a" "$TMP/b"
echo agent-ok >"$TMP/a/index.html"
echo other-port >"$TMP/b/index.html"
python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$TMP/a" >/dev/null 2>&1 &
PIDS+=("$!")
python3 -m http.server "$OTHER" --bind 127.0.0.1 --directory "$TMP/b" >/dev/null 2>&1 &
PIDS+=("$!")
for _ in $(seq 1 50); do
  curl -s "http://127.0.0.1:$PORT/" >/dev/null && curl -s "http://127.0.0.1:$OTHER/" >/dev/null && break
  sleep 0.1
done

wait_for() { # <command...>: retry for ten seconds
  for _ in $(seq 1 50); do "$@" >/dev/null 2>&1 && return 0; sleep 0.2; done
  return 1
}

echo
echo "-- 1. the restricted key opens no shell --"
out="$(as_key "$HOST" id 2>&1)"; rc=$?
if [ "$rc" -ne 0 ] && ! echo "$out" | grep -q 'uid='; then
  pass "ssh $HOST id fails (exit $rc) and prints no uid"
else
  fail "ssh $HOST id printed a uid or succeeded (exit $rc)"
fi
out="$(as_key "$HOST" 'echo shell-ran' 2>&1)"
echo "$out" | grep -q shell-ran \
  && fail "a command ran with the restricted key" \
  || pass "a second command did not run either"

echo
echo "-- 2. ssh -N -R $REMOTE_PORT:localhost:$PORT $HOST --"
as_key -N -o ExitOnForwardFailure=yes -R "$REMOTE_PORT:localhost:$PORT" "$HOST" &
PIDS+=("$!")
if wait_for as_me "curl -fs http://127.0.0.1:$REMOTE_PORT/ | grep -q agent-ok"; then
  pass "the stand-in answers on the loopback of $HOST at port $REMOTE_PORT through the -R forward"
else
  fail "nothing answered on port $REMOTE_PORT of $HOST through the -R forward"
fi
if [ "$HOST" != localhost ] && [ "$HOST" != 127.0.0.1 ] && [ "$HOST" != ::1 ]; then
  as_me "curl -fs --max-time 5 http://\$(hostname):$REMOTE_PORT/ >/dev/null 2>&1" \
    && fail "the forwarded port also answers on the host's own name (GatewayPorts on?)" \
    || pass "the forwarded port does not answer on the host's own name"
fi
# INFO: the same key, another remote port.
as_key -N -o ExitOnForwardFailure=yes -R "$((REMOTE_PORT + 1)):localhost:$PORT" "$HOST" &
EXTRA=$!
PIDS+=("$EXTRA")
if wait_for as_me "curl -fs http://127.0.0.1:$((REMOTE_PORT + 1))/ | grep -q agent-ok"; then
  echo "INFO  this key may -R forward port $((REMOTE_PORT + 1)) as well: the README options limit -L only; add permitlisten=\"localhost:$REMOTE_PORT\" to limit -R"
else
  echo "INFO  this key may not -R forward other ports"
fi
kill "$EXTRA" 2>/dev/null

echo
echo "-- 3. ssh -N -L $LPORT:localhost:$PORT $HOST --"
as_key -N -o ExitOnForwardFailure=yes -L "127.0.0.1:$LPORT:localhost:$PORT" "$HOST" &
PIDS+=("$!")
if wait_for sh -c "curl -fs http://127.0.0.1:$LPORT/ | grep -q agent-ok"; then
  pass "the -L forward reaches port $PORT on $HOST"
else
  fail "the -L forward does not reach port $PORT"
fi

echo
echo "-- 4. only port $PORT forwards with -L --"
as_key -N -L "127.0.0.1:$((LPORT + 1)):localhost:$OTHER" "$HOST" 2>/dev/null &
PIDS+=("$!")
sleep 2
if curl -fs --max-time 5 "http://127.0.0.1:$((LPORT + 1))/" | grep -q other-port; then
  fail "a -L forward to port $OTHER got through"
else
  pass "a -L forward to port $OTHER is refused (the stand-in on $OTHER is running)"
fi

echo
if [ "$FAILED" = 0 ]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; fi
exit "$FAILED"
