#!/usr/bin/env bash
# The nymira-mcp release: build the npm tarball twice from a tag, check both agree, sign the checksums offline.
#   scripts/release-mcp.sh build  nymira-mcp-vX.Y.Z   builds twice, writes dist/<tag>/ (tarball, RELEASE_NOTES.md, SHA256SUMS)
#   scripts/release-mcp.sh sign   nymira-mcp-vX.Y.Z   runs `minisign -S` on SHA256SUMS (asks for the key passphrase)
#   scripts/release-mcp.sh verify nymira-mcp-vX.Y.Z   checks the signature against web/pins/release-signing.pub, then the hashes
# This script never publishes: the team runs `npm publish dist/<tag>/nymira-mcp-X.Y.Z.tgz` by hand
# after `verify`, and attaches the three files to the GitHub release by hand. It never reads the
# secret key either; minisign asks for the passphrase itself.
# The tarball bundles the locked dependencies (bundleDependencies), so installing it needs no
# registry and the one hash covers everything that runs. Needs node >= 24, npm, git, minisign (sign, verify).
# Overrides, for tests: NYMIRA_MCP_REF (a commit instead of the tag), NYMIRA_MCP_DIST, NYMIRA_MCP_KEY, NYMIRA_MCP_PUB.
set -euo pipefail
# A parent `npm run` hands its settings down as npm_config_* variables; a release build reads only the project files.
for v in $(compgen -e | grep -i '^npm_config_' || true); do unset "$v"; done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEY="${NYMIRA_MCP_KEY:-${NYMIRA_RELEASE_KEY:-$HOME/nymira-release.key}}"
PUB="${NYMIRA_MCP_PUB:-$ROOT/web/pins/release-signing.pub}"
DIST_ROOT="${NYMIRA_MCP_DIST:-$ROOT/dist}"

cmd="${1:?build | sign | verify}"
tag="${2:?tag, as in nymira-mcp-v0.1.0}"
case "$tag" in nymira-mcp-v[0-9]*.[0-9]*.[0-9]*) ;; *) echo "tag must look like nymira-mcp-v0.1.0" >&2; exit 1 ;; esac
version="${tag#nymira-mcp-v}"
ref="${NYMIRA_MCP_REF:-refs/tags/$tag}"
dist="$DIST_ROOT/$tag"
TGZ="nymira-mcp-$version.tgz"

case "$cmd" in
build)
  commit="$(git -C "$ROOT" rev-parse -q --verify "$ref^{commit}")" || { echo "no $ref in this clone" >&2; exit 1; }
  work="$(mktemp -d "${TMPDIR:-/tmp}/nymira-mcp-release.XXXXXX")"; trap 'rm -rf "$work"' EXIT
  git -C "$ROOT" show "$commit:mcp/package.json" | grep -q "\"version\": \"$version\"" \
    || { echo "mcp/package.json at $commit is not version $version" >&2; exit 1; }
  build() { # $1 = output directory
    mkdir -p "$1/src" "$1/out"
    git -C "$ROOT" archive "$commit" mcp LICENSE-APACHE | tar -x -C "$1/src"
    cp "$1/src/LICENSE-APACHE" "$1/src/mcp/LICENSE"
    (cd "$1/src/mcp" && npm ci --ignore-scripts --no-audit --no-fund --prefer-offline >/dev/null \
      && node_modules/.bin/tsc -p tsconfig.build.json \
      && npm pack --ignore-scripts --pack-destination "$1/out" >/dev/null 2>&1)
    mv "$1/out/"*.tgz "$1/$TGZ"
  }
  build "$work/a"; build "$work/b"
  a="$(shasum -a 256 "$work/a/$TGZ" | cut -d' ' -f1)"; b="$(shasum -a 256 "$work/b/$TGZ" | cut -d' ' -f1)"
  echo "commit $commit"; echo "first build:  $a"; echo "second build: $b"
  [ "$a" = "$b" ] || { echo "two builds of one commit differ: not releasing" >&2; exit 1; }
  echo "two builds agree"
  mkdir -p "$dist"; rm -f "$dist/SHA256SUMS" "$dist/SHA256SUMS.minisig"
  cp "$work/a/$TGZ" "$dist/$TGZ"
  cat >"$dist/RELEASE_NOTES.md" <<NOTES
# @nymiralabs/nymira-mcp $version

- Source commit: \`$commit\`
- Package file: \`$TGZ\`
- SHA-256: \`$a\`
- Built with node $(node -v) and npm $(npm -v). Two builds of this commit gave the same file.

Check before you install: put \`$TGZ\`, \`SHA256SUMS\`, \`SHA256SUMS.minisig\` and \`release-signing.pub\`
(from \`web/pins/\` in the repository) in one directory, then

    minisign -V -p release-signing.pub -m SHA256SUMS
    shasum -a 256 -c SHA256SUMS

Both must pass. The tarball carries the locked dependencies, so the install needs no registry.
NOTES
  (cd "$dist" && shasum -a 256 "$TGZ" RELEASE_NOTES.md >SHA256SUMS)
  echo "built $dist"; cat "$dist/SHA256SUMS"
  echo "next: scripts/release-mcp.sh sign $tag   (on the machine that holds the release key)"
  ;;
sign)
  [ -f "$dist/SHA256SUMS" ] || { echo "run build first: $dist/SHA256SUMS is missing" >&2; exit 1; }
  minisign -S -s "$KEY" -m "$dist/SHA256SUMS" -x "$dist/SHA256SUMS.minisig" \
    -t "nymira-mcp-release v1 version=$version"
  echo "signed: $dist/SHA256SUMS.minisig"
  ;;
verify)
  [ -f "$dist/SHA256SUMS.minisig" ] || { echo "$dist/SHA256SUMS.minisig is missing: sign first" >&2; exit 1; }
  minisign -V -p "$PUB" -m "$dist/SHA256SUMS" -x "$dist/SHA256SUMS.minisig"
  # The sums must name both files, or one would go out unchecked.
  for f in "$TGZ" RELEASE_NOTES.md; do grep -q "  $f\$" "$dist/SHA256SUMS" || { echo "SHA256SUMS does not list $f" >&2; exit 1; }; done
  (cd "$dist" && shasum -a 256 -c SHA256SUMS)
  ;;
*) echo "unknown command $cmd" >&2; exit 1 ;;
esac
