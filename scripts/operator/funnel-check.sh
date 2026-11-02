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

echo "== streaming: chunks must arrive while the stream is open"
echo "Run a streamed chat through nymirad against this URL and watch that events arrive one by one;"
echo "a response that arrives all at once at the end means the entry buffers it (use non-streamed"
echo "responses, or the Cloudflare Tunnel). This part needs a lease, so it is not scripted here."
exit $fail
