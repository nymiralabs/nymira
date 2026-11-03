#!/usr/bin/env bash
# Rebuilds a web release in the pinned container and compares it with the release's manifest.
#   web/scripts/rebuild-in-container.sh <git-ref> <profile> <version> [out-dir]
# Needs Docker and a checkout that has <git-ref>. Leaves the rebuilt site in out-dir (default
# ./rebuilt-site) and exits 1 when its manifest differs from web/releases/<profile>-<version>/.
set -euo pipefail
ref="${1:?git ref (a tag or a commit)}"
profile="${2:?profile: testnet or mainnet}"
version="${3:?version, as in the release tag}"
out="${4:-rebuilt-site}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ctx="$(mktemp -d)"
trap 'rm -rf "$ctx"' EXIT

git -C "$root" archive "$ref" | tar -x -C "$ctx"
docker build --platform linux/amd64 -f "$ctx/web/Dockerfile.repro" \
  --build-arg "PROFILE=$profile" --build-arg "VERSION=$version" -t "nymira-web-rebuild:$version-$profile" "$ctx"
cid="$(docker create "nymira-web-rebuild:$version-$profile")"
rm -rf "$out"
docker cp "$cid:/src/web/site-$profile" "$out"
docker rm "$cid" >/dev/null
echo "rebuilt site: $out"
cat "$out/manifest.json"
signed="$ctx/web/releases/$profile-$version/manifest.json"
if [ -f "$signed" ]; then
  cmp "$out/manifest.json" "$signed" && echo "manifest.json is byte-for-byte the signed one"
else
  echo "no signed manifest at web/releases/$profile-$version/ in $ref: compare by hand" >&2
fi
