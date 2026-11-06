#!/usr/bin/env bash
# Runs the nymirad egress test under strace and fails on any connect() that leaves the machine:
# the only sockets allowed are loopback (the SOCKS proxy and the test's own sink), and none on a
# DNS port, since a name resolved here would show the name to the local resolver.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v strace >/dev/null || { echo "strace is required"; exit 2; }

bin=$(cargo test -p nymirad --test egress --no-run --message-format=json --locked \
  | python3 -c 'import sys,json
for l in sys.stdin:
    m=json.loads(l)
    if m.get("reason")=="compiler-artifact" and m.get("target",{}).get("name")=="egress" and m.get("executable"):
        print(m["executable"])')
[ -n "$bin" ] || { echo "no egress test binary"; exit 2; }

log=$(mktemp)
strace -f -qq -e trace=connect -s 64 -o "$log" "$bin" --test-threads=1

archive_bin=$(cargo test -p nymirad --test intents_acceptance --no-run --message-format=json --locked \
  | python3 -c 'import sys,json
for l in sys.stdin:
    m=json.loads(l)
    if m.get("reason")=="compiler-artifact" and m.get("target",{}).get("name")=="intents_acceptance" and m.get("executable"):
        print(m["executable"])')
[ -n "$archive_bin" ] || { echo "no archival recovery test binary"; exit 2; }
archive_log=$(mktemp)
strace -f -qq -e trace=connect -s 64 -o "$archive_log" "$archive_bin" \
  --exact an_aged_receipt_and_the_pending_account_balance_are_read_over_socks --test-threads=1
cat "$archive_log" >> "$log"
rm -f "$archive_log"

python3 - "$log" <<'PY'
import re, sys
bad, inet = [], 0
for line in open(sys.argv[1]):
    # `connect(` or, with -f, `<... connect resumed>`; AF_UNIX and the like are not network.
    if "connect" not in line or "sa_family=AF_INET" not in line:
        continue
    port = re.search(r'sin6?_port=htons\((\d+)\)', line)
    addr = re.search(r'inet_addr\("([^"]+)"\)|inet_pton\(AF_INET6, "([^"]+)"', line)
    if not (port and addr):
        bad.append("unparsed: " + line.strip())
        continue
    inet += 1
    ip = addr.group(1) or addr.group(2)
    if not (ip.startswith("127.") or ip == "::1") or int(port.group(1)) in (53, 853):
        bad.append(line.strip())
if bad:
    print("connections that leave the machine, reach a resolver or could not be read:")
    print("\n".join(bad))
    sys.exit(1)
if inet == 0:
    print("strace saw no inet connect(): the check did not run")
    sys.exit(1)
print(f"egress ok: {inet} connections, all loopback, none to a DNS port")
PY
