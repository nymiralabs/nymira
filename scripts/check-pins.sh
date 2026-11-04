#!/usr/bin/env bash
# The vault's release verifying-key hashes (contracts/vault/src/pins.rs, the non-dev block) must
# equal the vendored CEREMONY_VK_KECCAK256. Both change at the ceremony; this fails when only one
# of them was edited. It also refuses zkapi's development hashes in a release block, and, with
# --final, the rehearsal ceremony's hashes too (run it so once the final ceremony is imported).
# Where the web testnet profile pins the proving keys, the hashes must equal the ones `nymirad`
# compiles in.
#   scripts/check-pins.sh [--final]
set -euo pipefail
FINAL=0; [ "${1:-}" = "--final" ] && FINAL=1; export FINAL
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

# The development block of the vault is zkapi's dev setup: never a release value.
dev = open("contracts/vault/src/pins.rs").read()
dev = block(dev, "#[cfg(any(feature = \"dev-setup\", test))]", "\n};")
rehearsal = {
    "request": "6d359ba4de593e4ed1c6ee823adf1a2a05efa97cab454c5a1c801497556c1ea7",
    "withdrawal": "da1f2c68e4ca28feaeac7f02f12702250ed600a01f90aee06b1a29658f15f7fe",
}
import os
for field in ("request", "withdrawal"):
    have = bytes(bytes_of(vault, field)).hex()
    if have == bytes(bytes_of(dev, field)).hex():
        sys.exit(f"{field}: the release pins hold zkapi's development hash")
    if os.environ.get("FINAL") == "1" and have == rehearsal[field]:
        sys.exit(f"{field}: --final, but the release pins still hold the rehearsal hash")
print("vault release pins equal the vendored ceremony hashes")

# The proving-key hashes `nymirad` compiles in against the web profile, when the profile has them.
def web_hash(name):
    t = open("web/src/profiles/testnet.ts").read()
    m = re.search(name + r":\s*\{[^}]*sha256:\s*'([0-9a-f]{64})'", t)
    return m.group(1) if m else None

for name, pin in (("provingKey", "request"), ("withdrawalKey", "withdrawal")):
    web = web_hash(name)
    path = "crates/nymirad/pins/proving-key.sha256" if pin == "request" else "crates/nymirad/pins/withdrawal-key.sha256"
    nymirad = open(path).read().strip()
    if web is not None and web != nymirad:
        sys.exit(f"{name}: the web testnet profile and {path} differ")
    if os.environ.get("FINAL") == "1" and nymirad in (
        "ba2d6ba6cb79d579f6ab1d9de28a022a525f1aba1853f5591ffff1f224932d8b",
        "8c44b059baf9152079810e3f40a157f19812df89c500560cc2509af51d1c2bab",
    ):
        sys.exit(f"{path}: --final, but it still holds the rehearsal hash")
print("proving-key pins agree" if True else "")
PY
