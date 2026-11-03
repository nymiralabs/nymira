#!/usr/bin/env bash
# The vault's release verifying-key hashes (contracts/vault/src/pins.rs, the non-dev block) must
# equal the vendored CEREMONY_VK_KECCAK256. Both change at the ceremony; this fails when only one
# of them was edited.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import re, sys

def block(text, start, end):
    i = text.index(start)
    j = text.index(end, i)
    return text[i:j]

def bytes_of(chunk, field):
    m = re.search(field + r":\s*\[(.*?)\]", chunk, re.S)
    if not m:
        sys.exit(f"no {field} array found")
    return [int(x, 16) for x in re.findall(r"0x([0-9a-fA-F]{2})", m.group(1))]

vault = open("contracts/vault/src/pins.rs").read()
vault = block(vault, "#[cfg(not(any(feature = \"dev-setup\", test)))]", "\n};")
zk = open("vendor/zkapi/protocol/rust/crates/zkapi-proof/src/compact.rs").read()
zk = block(zk, "pub const CEREMONY_VK_KECCAK256", "\n});")
for field in ("request", "withdrawal"):
    a, b = bytes_of(vault, field), bytes_of(zk, field)
    if len(a) != 32 or a != b:
        sys.exit(f"{field}: vault pins differ from the vendored CEREMONY_VK_KECCAK256")
print("vault release pins equal the vendored ceremony hashes")
PY
