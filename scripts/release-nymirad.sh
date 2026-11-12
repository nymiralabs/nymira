#!/usr/bin/env bash
# The nymirad release: build in a pinned container, sign the checksums offline, publish.
#   scripts/release-nymirad.sh build   nymirad-vX.Y.Z   builds twice, checks the two agree, writes dist/nymirad-vX.Y.Z/
#   scripts/release-nymirad.sh sign    nymirad-vX.Y.Z   runs `minisign -S` on SHA256SUMS (asks for the key passphrase)
#   scripts/release-nymirad.sh add-macos nymirad-vX.Y.Z PATH   adds the macOS arm64 binary (built by the nymirad-macos workflow), rewrites SHA256SUMS, drops the old signature: run `sign` again
#   scripts/release-nymirad.sh verify  nymirad-vX.Y.Z   checks the signature against web/pins/release-signing.pub, then the hashes
#   scripts/release-nymirad.sh publish nymirad-vX.Y.Z   verifies, then creates the GitHub release (as nymiralabs)
# Needs Docker (linux/amd64 image), minisign, gh. This script never reads the secret key: it
# hands the file's path to minisign, which asks for the passphrase itself. Run `sign` on the
# machine that holds the key (the key stays offline); the other steps need no key.
# Only the linux x86_64 binary is built here, because only it is reproducible; see web/INTEGRITY.md.
# The macOS arm64 binary comes from CI and goes in with `add-macos`: signed, but not reproducible.
set -euo pipefail

# The same image as web/Dockerfile.repro: rust 1.99.0 on bookworm, pinned by digest.
IMAGE="rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6"
KEY="${NYMIRA_RELEASE_KEY:-$HOME/nymira-release.key}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUB="$ROOT/web/pins/release-signing.pub"

cmd="${1:?build | add-macos | sign | verify | publish}"
tag="${2:?tag, as in nymirad-v0.1.0}"
case "$tag" in nymirad-v[0-9]*.[0-9]*.[0-9]*) ;; *) echo "tag must look like nymirad-v0.1.0" >&2; exit 1 ;; esac
version="${tag#nymirad-v}"
dist="$ROOT/dist/$tag"
BIN_NAME="nymirad-$version-x86_64-linux"

MAC_NAME="nymirad-$version-aarch64-macos"

# Every file that ships, in a fixed order (the macOS binary only when it is there).
release_files() {
  echo "$BIN_NAME"
  [ -f "$dist/$MAC_NAME" ] && echo "$MAC_NAME"
  echo attestation-policy.json
  echo TERMS.md
  return 0
}
write_sums() {
  # bash 3.2 (macOS) has no mapfile
  local files=() f
  while IFS= read -r f; do files+=("$f"); done < <(release_files)
  (cd "$dist" && shasum -a 256 "${files[@]}" >SHA256SUMS)
  rm -f "$dist/SHA256SUMS.minisig"
}
# The sums must name every file that ships, or a file would go out unchecked.
sums_ok() {
  local f
  for f in $(release_files); do
    grep -q "  $f\$" "$dist/SHA256SUMS" || { echo "SHA256SUMS does not list $f" >&2; return 1; }
  done
  (cd "$dist" && shasum -a 256 -c SHA256SUMS)
}

case "$cmd" in
build)
  # A tag, not a branch: the published hashes name this commit.
  git -C "$ROOT" rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "no tag $tag in this clone" >&2; exit 1; }
  # Colima shares only $HOME with its VM, so the build directory must live there.
  if [ "$(uname)" = Darwin ]; then export TMPDIR="$HOME/.cache/nymira-tmp"; mkdir -p "$TMPDIR"; fi
  work="$(mktemp -d "${TMPDIR:-/tmp}/nymirad-release.XXXXXX")"; trap 'rm -rf "$work"' EXIT
  mkdir -p "$work/src" "$dist"
  git -C "$ROOT" archive "$tag" | tar -x -C "$work/src"
  commit="$(git -C "$ROOT" rev-parse "$tag^{commit}")"
  epoch="$(git -C "$ROOT" log -1 --format=%ct "$commit")"
  # Each build gets its own empty cargo registry. A second build that reuses the first one's
  # warm registry gives a different binary (measured 2026-11-04: two cold builds were equal, the
  # warm one differed), and anyone rebuilding from scratch starts cold.
  build() { # $1 = output directory
    mkdir -p "$1" "$1.registry"
    # Path remapping and a fixed epoch keep the host's paths and clock out of the binary. One job:
    # with two, builds of one commit came out in two different forms (measured 2026-11-12, `ring` and
    # `libsqlite3-sys` compile C in parallel); four builds with one job were identical.
    docker run --rm --platform linux/amd64 \
      -v "$work/src:/src:ro" -v "$1.registry:/usr/local/cargo/registry" -v "$1:/out" \
      -e SOURCE_DATE_EPOCH="$epoch" -e CARGO_TARGET_DIR=/tmp/target -e CARGO_BUILD_JOBS=1 \
      -e RUSTFLAGS="--remap-path-prefix=/src=/nymira --remap-path-prefix=/usr/local/cargo=/cargo" \
      -w /src "$IMAGE" \
      sh -c 'cargo build --release --locked -p nymirad --bin nymirad && cp /tmp/target/release/nymirad /out/nymirad'
  }
  # The two builds run side by side to halve the wait.
  build "$work/a" & pid_a=$!
  build "$work/b" & pid_b=$!
  wait "$pid_a" || { echo "the first build failed" >&2; exit 1; }
  wait "$pid_b" || { echo "the second build failed" >&2; exit 1; }
  a="$(shasum -a 256 "$work/a/nymirad" | cut -d' ' -f1)"; b="$(shasum -a 256 "$work/b/nymirad" | cut -d' ' -f1)"
  echo "commit $commit"; echo "first build:  $a"; echo "second build: $b"
  [ "$a" = "$b" ] || { echo "two builds of one commit differ: not publishing" >&2; exit 1; }
  # No mock feature and no demo strings (the string scan reads the binary; the feature graph reads the tag's tree).
  (cd "$work/src" && scripts/check-release-binary.sh "$work/a/nymirad")
  cp "$work/a/nymirad" "$dist/$BIN_NAME"
  cp "$work/src/crates/nymirad/pins/attestation-policy.json" "$work/src/TERMS.md" "$dist/"
  write_sums
  echo "built $dist"; cat "$dist/SHA256SUMS"
  echo "next: scripts/release-nymirad.sh sign $tag   (on the machine that holds the release key)"
  ;;
add-macos)
  mac="${3:?path to the macOS binary}"
  [ -f "$mac" ] || { echo "$mac is not a file" >&2; exit 1; }
  [ -f "$dist/$BIN_NAME" ] || { echo "run build first: $dist/$BIN_NAME is missing" >&2; exit 1; }
  cp "$mac" "$dist/$MAC_NAME"
  write_sums
  echo "added $MAC_NAME (signed, but not reproducible: the release notes must say so)"; cat "$dist/SHA256SUMS"
  echo "next: scripts/release-nymirad.sh sign $tag   (the old signature is removed: sign again)"
  ;;
sign)
  [ -f "$dist/SHA256SUMS" ] || { echo "run build first: $dist/SHA256SUMS is missing" >&2; exit 1; }
  # minisign asks for the passphrase on the terminal; this script does not touch the key file.
  minisign -S -s "$KEY" -m "$dist/SHA256SUMS" -x "$dist/SHA256SUMS.minisig" \
    -t "nymira-nymirad-release v1 version=$version"
  echo "signed: $dist/SHA256SUMS.minisig"
  ;;
verify)
  [ -f "$dist/SHA256SUMS.minisig" ] || { echo "$dist/SHA256SUMS.minisig is missing: sign first" >&2; exit 1; }
  minisign -V -p "$PUB" -m "$dist/SHA256SUMS" -x "$dist/SHA256SUMS.minisig"
  sums_ok
  ;;
publish)
  "$0" verify "$tag"
  git -C "$ROOT" ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null || { echo "push the tag first" >&2; exit 1; }
  assets=("$dist/$BIN_NAME")
  [ -f "$dist/$MAC_NAME" ] && assets+=("$dist/$MAC_NAME")
  GH_CONFIG_DIR="${GH_CONFIG_DIR:-$HOME/.config/gh-nymiralabs}" gh release create "$tag" \
    --repo nymiralabs/nymira --title "nymirad $version" --notes "Verify with minisign and web/pins/release-signing.pub, then sha256 -c SHA256SUMS. See web/INTEGRITY.md." \
    "${assets[@]}" "$dist/attestation-policy.json" "$dist/TERMS.md" "$dist/SHA256SUMS" "$dist/SHA256SUMS.minisig"
  ;;
*) echo "unknown command $cmd" >&2; exit 1 ;;
esac
