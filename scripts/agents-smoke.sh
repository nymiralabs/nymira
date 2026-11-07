#!/usr/bin/env bash
# The agents smoke test: the pinned IronClaw release against the mock stack.
#
#   scripts/agents-smoke.sh
#
# It downloads IronClaw (the release named in scripts/agents-ironclaw.sha256) and checks the
# file's hash before it unpacks it, starts the `make demo` stack, makes a budget note through the
# real `nymirad` binary, signs and loads the demo policy with the demo's test owner key
# (`demo_sign`, a mock-only example), issues the agent token, starts `nymirad serve`, and then:
#
#   1. asks `nymirad` over HTTP with the agent token: model list, one streamed answer, one answer
#      with a tool definition, a model outside the policy (402 `model_not_allowed`);
#   2. runs IronClaw as a separate OS user with the recipe's environment file, asks one question,
#      then asks for the disallowed model and checks that IronClaw's output carries the 402 reason;
#   3. checks with `nymirad agent show` that a lease was charged to the budget note.
#
# Needs: bash, curl, a tar, sha256sum or shasum, `sudo` for a second OS user (CI has it), the
# toolchain from rust-toolchain.toml and cargo-near 0.22.0 (the demo builds a contract), and port
# 8788 free on loopback. Nothing here reaches a real network except the IronClaw download.
#
# Environment (all optional):
#   AGENT_USER=nymira-agent   the OS user IronClaw runs as; created with sudo if it does not exist
#   SMOKE_SAME_USER=1         run IronClaw as yourself with its own HOME (a weaker local run: the
#                             agent then shares your OS user, which the recipe forbids)
#   NYMIRAD_PORT=8788         the port `serve` binds. IronClaw reaches only 8788, so any other
#                             value skips the IronClaw steps (the HTTP steps still run)
#   NO_BUILD=1                use the binaries already in target/debug

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAG="ironclaw-v1.4.1"
PORT="${NYMIRAD_PORT:-8788}"
AGENT_USER="${AGENT_USER:-nymira-agent}"
MODEL="z-ai/glm-5.3-flash"
NAME="ironclaw"
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug"

step() { printf '\n== %s\n' "$*"; }
# SMOKE_KEEP=1 leaves the stack running after a failure, for a look by hand (Ctrl-C ends it).
die() {
  printf 'FAIL: %s\n' "$*" >&2
  if [ -n "${SMOKE_KEEP:-}" ]; then echo "stack kept; state in ${DEMO_HOME:-?}" >&2; sleep 86400; fi
  exit 1
}
ok() { printf 'ok: %s\n' "$*"; }

WORK="$(mktemp -d)"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

sha256_of() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# --- 1. IronClaw, by hash ------------------------------------------------------------------
step "IronClaw $TAG: download and hash check"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  *) die "no pinned IronClaw build for $(uname -s)-$(uname -m)" ;;
esac
ARCHIVE="ironclaw-$TARGET.tar.gz"
WANT="$(grep "  $ARCHIVE\$" "$ROOT/scripts/agents-ironclaw.sha256" | cut -d' ' -f1)"
[ -n "$WANT" ] || die "scripts/agents-ironclaw.sha256 has no line for $ARCHIVE"
curl --proto '=https' --tlsv1.2 -fsSL -o "$WORK/$ARCHIVE" \
  "https://github.com/nearai/ironclaw/releases/download/$TAG/$ARCHIVE"
[ "$(sha256_of "$WORK/$ARCHIVE")" = "$WANT" ] || die "$ARCHIVE does not match the pinned hash"
tar -xzf "$WORK/$ARCHIVE" -C "$WORK"
IRONCLAW="$WORK/ironclaw-$TARGET/ironclaw"
[ "$("$IRONCLAW" --version)" = "ironclaw 1.4.1" ] || die "unexpected IronClaw version"
ok "hash matches, $("$IRONCLAW" --version)"

# --- 2. the mock stack ---------------------------------------------------------------------
if [ -z "${NO_BUILD:-}" ]; then
  step "build nymirad, the demo and the demo signer (mock)"
  (cd "$ROOT" && cargo build --locked -p nymirad --features mock --bin nymirad --example demo --example demo_sign)
fi

step "start the demo stack"
"$BIN/examples/demo" >"$WORK/demo.log" 2>&1 &
PIDS+=($!)
DEMO_PID=$!
for _ in $(seq 1 600); do
  grep -q 'Demo stack is up' "$WORK/demo.log" && break
  kill -0 "$DEMO_PID" 2>/dev/null || break
  sleep 1
done
grep -q 'Demo stack is up' "$WORK/demo.log" || { tail -20 "$WORK/demo.log"; die "the demo did not come up"; }
DEMO_HOME="$(sed -n 's/.* --home \(.*\) --passphrase-file .*/\1/p' "$WORK/demo.log" | head -1)"
[ -d "$DEMO_HOME" ] || die "cannot find the demo state directory"
# The demo operator serves the testnet chain, and a mock build reads the demo vault's pins first,
# so `serve` runs in testnet mode. Only the deposit needs mainnet mode: it refuses a token that
# is not Circle's testnet USDC, and the demo's is a mock.
nd() { NYMIRA_MODE="${MODE:-testnet}" "$BIN/nymirad" --home "$DEMO_HOME" --passphrase-file "$DEMO_HOME/passphrase" "$@"; }
ok "demo state in $DEMO_HOME"

step "budget note, signed demo policy, agent token"
TOKEN_FILE="$DEMO_HOME/$NAME.token"
echo yes | MODE=mainnet nd agent create --name "$NAME" --transport tcp --token-file "$TOKEN_FILE" \
  --key-file "$DEMO_HOME/funding.key" --amount 1000000 >"$WORK/create.log" 2>&1 \
  || { cat "$WORK/create.log"; die "agent create failed"; }
"$BIN/examples/demo_sign" "$DEMO_HOME" "$NAME" 1
echo yes | nd agent policy --name "$NAME" --file "$DEMO_HOME/$NAME-policy.signed.json" \
  >"$WORK/policy.log" 2>&1 || { cat "$WORK/policy.log"; die "agent policy failed"; }
grep -q "Policy v1 is in force" "$WORK/policy.log" || { cat "$WORK/policy.log"; die "the policy is not in force"; }
TOKEN="$(cat "$TOKEN_FILE")"
case "$TOKEN" in nyma_*) ok "agent token issued, policy v1 in force" ;; *) die "no nyma_ token" ;; esac

step "nymirad serve"
if (exec 3<>/dev/tcp/127.0.0.1/"$PORT") 2>/dev/null; then die "port $PORT is already in use"; fi
# Started directly, not through `nd`, so that $! is the daemon and cleanup stops it.
NYMIRA_MODE=testnet "$BIN/nymirad" --home "$DEMO_HOME" --passphrase-file "$DEMO_HOME/passphrase" \
  serve --port "$PORT" >"$WORK/serve.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 120); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: localhost:8788' "http://127.0.0.1:$PORT/v1/models" || true)" = 401 ] && break
  sleep 1
done

# The listener accepts only `Host: localhost:8788` whatever port a mock build binds.
api() { local path="$1"; shift; curl -sS -m 300 -H 'Host: localhost:8788' -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$PORT$path" "$@"; }
chat() { api /v1/chat/completions -H 'Content-Type: application/json' -w '\n%{http_code}' -d "$1"; }

# --- 3. the agent API over HTTP ------------------------------------------------------------
step "models, a streamed answer, a tool definition, a model outside the policy"
api /v1/models | grep -q "\"id\":\"$MODEL\"" || die "/v1/models does not list $MODEL"
[ "$(api /v1/models | grep -o '"id"' | wc -l | tr -d ' ')" = 1 ] || die "/v1/models lists more than the policy allows"
ok "/v1/models lists only $MODEL"

# The demo operator moves a new note into its tree every 10 seconds (until then a lease is refused
# with 402), and a new lease can wait for the operator's admission (429 `operator_capacity`).
for _ in $(seq 1 30); do
  out="$(chat "{\"model\":\"$MODEL\",\"max_tokens\":20,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}")"
  case "$(printf '%s' "$out" | tail -1)" in 402 | 429) sleep 5 ;; *) break ;; esac
done
[ "$(printf '%s' "$out" | tail -1)" = 200 ] || { echo "$out"; die "the first answer through the budget note failed"; }
ok "first answer (a lease opened on the budget note)"

out="$(chat "{\"model\":\"$MODEL\",\"stream\":true,\"max_tokens\":50,\"messages\":[{\"role\":\"user\",\"content\":\"say hello\"}]}")"
[ "$(printf '%s' "$out" | tail -1)" = 200 ] || { echo "$out"; die "streamed chat: not 200"; }
printf '%s' "$out" | grep -q '^data: ' && printf '%s' "$out" | grep -q 'data: \[DONE\]' || { echo "$out"; die "streamed chat: no event stream"; }
ok "streamed answer arrived as an event stream"

tools='[{"type":"function","function":{"name":"get_time","description":"The time","parameters":{"type":"object","properties":{}}}}]'
out="$(chat "{\"model\":\"$MODEL\",\"max_tokens\":50,\"tools\":$tools,\"messages\":[{\"role\":\"user\",\"content\":\"what time is it\"}]}")"
[ "$(printf '%s' "$out" | tail -1)" = 200 ] || { echo "$out"; die "chat with a tool definition: not 200"; }
printf '%s' "$out" | grep -q '"charge_usd"' || { echo "$out"; die "no nymira.charge_usd in the answer"; }
ok "an answer with a tool definition in the request, charge shown"

out="$(chat "{\"model\":\"not/in-the-policy\",\"max_tokens\":50,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}")"
[ "$(printf '%s' "$out" | tail -1)" = 402 ] && printf '%s' "$out" | grep -q '"model_not_allowed"' \
  || { echo "$out"; die "a model outside the policy was not refused with 402 model_not_allowed"; }
ok "402 model_not_allowed in OpenAI's error shape"

# --- 4. IronClaw as a separate user --------------------------------------------------------
if [ "$PORT" != 8788 ]; then
  echo "NYMIRAD_PORT is $PORT: IronClaw reaches only 8788, so its steps are skipped"
  if [ -n "${SMOKE_KEEP:-}" ]; then echo "stack kept; state in $DEMO_HOME, token in $TOKEN_FILE" >&2; sleep 86400; fi
  exit 0
fi

step "IronClaw as $AGENT_USER"
if [ -n "${SMOKE_SAME_USER:-}" ]; then
  AGENT_HOME="$WORK/agent-home"; mkdir -p "$AGENT_HOME"
  as_agent() { env -i PATH="$PATH" HOME="$AGENT_HOME" "$@"; }
  install_file() { install -m 600 /dev/stdin "$1"; }
  chmod 755 "$WORK"
else
  id "$AGENT_USER" >/dev/null 2>&1 || sudo useradd --create-home --shell /bin/bash "$AGENT_USER"
  AGENT_HOME="$(eval echo "~$AGENT_USER")"
  as_agent() { sudo -u "$AGENT_USER" -H env -i PATH="$PATH" HOME="$AGENT_HOME" "$@"; }
  install_file() { sudo install -o "$AGENT_USER" -m 600 /dev/stdin "$1"; }
  # The agent user must be able to run the unpacked binary.
  chmod -R a+rX "$WORK"
fi

# The recipe's environment file: only the agent's user can read it.
install_file "$AGENT_HOME/nymira.env" <<EOF
LLM_BACKEND=openai_compatible
LLM_BASE_URL=http://localhost:8788/v1
LLM_API_KEY=$TOKEN
LLM_MODEL=$MODEL
EOF
as_agent mkdir -p "$AGENT_HOME/work"
ic() { (cd "$AGENT_HOME/work" && as_agent bash -c 'set -a; . "$HOME/nymira.env"; set +a; exec "$@"' _ "$IRONCLAW" "$@"); }
ic onboard --no-service </dev/null >"$WORK/onboard.log" 2>&1 || { cat "$WORK/onboard.log"; die "ironclaw onboard failed"; }
ic models status </dev/null | grep -q "default.base_url: http://localhost:8788/v1" || die "IronClaw does not point at nymirad"
ok "IronClaw is set to nymirad on loopback"

ic run --message "Reply with one short greeting." </dev/null >"$WORK/ic-answer.out" 2>"$WORK/ic-answer.err" \
  || { tail -20 "$WORK/ic-answer.err"; die "ironclaw run failed"; }
[ -n "$(sed 's/\x1b\[[0-9;]*m//g' "$WORK/ic-answer.out" | grep -v '^$' | tail -1)" ] || die "IronClaw printed no answer"
ok "IronClaw got an answer through nymirad"

# The same user, a model the policy does not allow.
ic_bad() { (cd "$AGENT_HOME/work" && as_agent bash -c 'set -a; . "$HOME/nymira.env"; LLM_MODEL=not/in-the-policy; set +a; exec "$@"' _ "$IRONCLAW" "$@"); }
if ic_bad run --message "hello" </dev/null >"$WORK/ic-bad.out" 2>"$WORK/ic-bad.err"; then die "IronClaw got an answer for a model outside the policy"; fi
cat "$WORK/ic-bad.out" "$WORK/ic-bad.err" | grep -q 'model_not_allowed' || { tail -20 "$WORK/ic-bad.err"; die "IronClaw's output does not carry the 402 reason"; }
ok "IronClaw shows the 402 reason (model_not_allowed)"

# --- 5. the charge -------------------------------------------------------------------------
step "the owner command shows the charge"
# A lease is charged when it settles; wait for the settle loop.
for _ in $(seq 1 90); do
  nd agent show >"$WORK/show.log" 2>&1 || true
  grep -Eq 'spent today[^0-9]*0\.0*[1-9]|spent today[^0-9]*[1-9]' "$WORK/show.log" && break
  sleep 2
done
cat "$WORK/show.log"
grep -Eq 'spent today[^0-9]*0\.0*[1-9]|spent today[^0-9]*[1-9]' "$WORK/show.log" || die "agent show shows no charge for the budget note"
ok "a lease was charged to the budget note"
echo; echo "agents smoke test passed"
