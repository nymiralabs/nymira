#!/bin/bash
# Checks what Tailscale Funnel (or a Cloudflare Tunnel) does to the browser-facing routes:
# the OPTIONS preflight and a response that streams. Needs the public URL of a running operator.
# usage: scripts/operator/funnel-check.sh https://nymira-operator.<tailnet>.ts.net https://nymira.pages.dev
set -u
URL="${1:?operator url}"
ORIGIN="${2:?web origin}"
fail=0

echo "== preflight"
headers=$(curl -sS -i -X OPTIONS "$URL/v1/chat/completions" \
  -H "Origin: $ORIGIN" -H "Access-Control-Request-Method: POST" \
  -H "Access-Control-Request-Headers: content-type,x-nymira-lease-token,x-nymira-lease-id")
echo "$headers" | head -20
echo "$headers" | grep -qi "^access-control-allow-origin: $ORIGIN" || { echo "FAIL: no exact allow-origin"; fail=1; }
echo "$headers" | grep -qi "^access-control-allow-origin: \*" && { echo "FAIL: wildcard origin"; fail=1; }

echo "== another origin gets no allow header"
other=$(curl -sS -i -X OPTIONS "$URL/v1/status" -H "Origin: https://evil.example" -H "Access-Control-Request-Method: GET")
echo "$other" | grep -qi "^access-control-allow-origin" && { echo "FAIL: allowed a foreign origin"; fail=1; }

echo "== status over the entry"
curl -sS "$URL/v1/status" | head -c 300; echo

echo "== streaming: events must arrive one by one while the stream is open"
echo "Serve a stream and read it through the entry. The example makes one (an event a second at /sse"
echo "on 127.0.0.1:8081, next to a local operator stack on 8080):"
echo "  cargo run -p nymira-operatord --features dev-setup --example local_stack"
echo "  tailscale funnel --bg --https=8443 8081"
echo "  curl -sN $URL:8443/sse | while read -r l; do echo \"\$(date +%S) \$l\"; done"
echo "Timestamps one second apart mean the entry streams; all at the end means it buffers."
exit $fail
