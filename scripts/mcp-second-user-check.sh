#!/usr/bin/env bash
set -euo pipefail

# Run the built server as a different OS user against a group-readable owner token file.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
chmod 755 "$WORK"
cp -R "$ROOT/mcp/dist" "$ROOT/mcp/node_modules" "$ROOT/mcp/package.json" "$WORK/"
chmod -R a+rX "$WORK"
TOKEN_FILE="$WORK/agent.token"
printf 'nyma_%064d\n' 0 > "$TOKEN_FILE"
sudo chgrp "$(id -gn nobody)" "$TOKEN_FILE"
chmod 640 "$TOKEN_FILE"
NODE="$(command -v node)"
python3 - "$WORK" "$NODE" "$TOKEN_FILE" <<'PY'
import json, os, subprocess, sys
work, node, token_file = sys.argv[1:]
command = ["sudo", "-n", "-u", "nobody", node, os.path.join(work, "dist/index.js"),
           "--transport", "tcp", "--url", "http://127.0.0.1:8788", "--token-file", token_file]
request = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}) + "\n"
result = subprocess.run(command, input=request, text=True, capture_output=True, timeout=30)
assert result.returncode == 0, result.stderr
assert json.loads(result.stdout)["result"]["serverInfo"]["name"] == "nymira-mcp"
assert "nyma_" not in result.stdout + result.stderr
os.chmod(token_file, 0o600)
denied = subprocess.run(command, input=request, text=True, capture_output=True, timeout=30)
assert denied.returncode != 0 and "cannot be opened" in denied.stderr, denied.stderr
assert "nyma_" not in denied.stdout + denied.stderr
print("second OS user: 0640 accepted, 0600 refused; token not printed")
PY
