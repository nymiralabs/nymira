#!/usr/bin/env bash
# Restore a missing signed side policy after an unsuccessful web deployment.
set -euo pipefail
tag="$(git tag --list 'web-v*' --sort=-version:refname | head -1)"
[ -n "$tag" ] || { echo 'No web release tag' >&2; exit 1; }
bundled="$(git show "$tag:web/pins/attestation-policy.json" | node -e 'console.log(JSON.parse(require("fs").readFileSync(0,"utf8")).version)')"
current="$(node -e 'console.log(JSON.parse(require("fs").readFileSync("web/pins/attestation-policy.json","utf8")).version)')"
if [ "$current" -le "$bundled" ]; then exit 0; fi
live="$(mktemp)"
trap 'rm -f "$live"' EXIT
missing=false
for url in https://nymira.pages.dev/attestation-policy.json https://nymiralabs.github.io/nymira/attestation-policy.json; do
  status="$(curl -sS --max-time 20 --retry 2 -o "$live" -w '%{http_code}' "$url")"
  case "$status" in
    200)
      if cmp -s "$live" web/pins/attestation-policy.json; then continue; fi
      live_version="$(node -e 'try { const v=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).version; console.log(Number.isSafeInteger(v) ? v : 0); } catch { console.log(0); }' "$live")"
      [ "$live_version" -le "$current" ] || { echo 'Live policy is newer than this checkout' >&2; exit 1; }
      missing=true
      ;;
    404) missing=true ;;
    *) echo "Cannot check live policy: HTTP $status" >&2; exit 1 ;;
  esac
done
[ "$missing" = true ] || exit 0
active="$(gh run list --repo "$GITHUB_REPOSITORY" --workflow web-deploy.yml --limit 100 --json status --jq 'any(.[]; .status != "completed")')"
if [ "$active" = true ]; then echo 'A web deployment is already in progress'; exit 0; fi
sha="$(git rev-parse HEAD)"
remote="$(git ls-remote origin refs/heads/main | cut -f1)"
[ "$sha" = "$remote" ] || { echo 'Main changed during the check; try again on the next run' >&2; exit 1; }
echo "The current policy is not live; starting a deployment for $tag"
gh workflow run web-deploy.yml --repo "$GITHUB_REPOSITORY" \
  -f ref="$tag" -f profile=testnet -f version="${tag#web-v}" -f policy_ref="$sha"
