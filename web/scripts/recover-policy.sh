#!/usr/bin/env bash
# Restore a missing signed side policy after an unsuccessful web deployment.
set -euo pipefail
tag="$(git tag --list 'web-v*' --sort=-version:refname | head -1)"
[ -n "$tag" ] || { echo 'No web release tag' >&2; exit 1; }
bundled="$(git show "$tag:web/pins/attestation-policy.json" | node -e 'console.log(JSON.parse(require("fs").readFileSync(0,"utf8")).version)')"
current="$(node -e 'console.log(JSON.parse(require("fs").readFileSync("web/pins/attestation-policy.json","utf8")).version)')"
live="$(mktemp)"
expected="$(mktemp)"
trap 'rm -f "$live" "$expected"' EXIT
if [ "$current" -le "$bundled" ]; then
  git show "$tag:web/releases/testnet-${tag#web-v}/manifest.json" > "$expected"
  path=manifest.json
else
  cp web/pins/attestation-policy.json "$expected"
  path=attestation-policy.json
fi
missing=false
for host in https://nymira.pages.dev https://nymiralabs.github.io/nymira; do
  if [ "$path" = manifest.json ]; then
    side_status="$(curl -sS --max-time 20 --retry 2 -o "$live" -w '%{http_code}' "$host/attestation-policy.json")" || side_status=network_error
    case "$side_status" in
      200)
        side_version="$(node -e 'try { const v=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).version; console.log(Number.isSafeInteger(v) ? v : 0); } catch { console.log(0); }' "$live")"
        [ "$side_version" -le "$current" ] || { echo 'Live policy is newer than this checkout' >&2; exit 1; }
        ;;
      404) ;;
      *)
        echo "Cannot check $host policy: $side_status" >&2
        if [ "$host" = https://nymira.pages.dev ] || [ "$missing" = false ]; then exit 1; fi
        continue
        ;;
    esac
  fi
  status="$(curl -sS --max-time 20 --retry 2 -o "$live" -w '%{http_code}' "$host/$path")" || status=network_error
  case "$status" in
    200)
      if cmp -s "$live" "$expected"; then continue; fi
      if [ "$path" = attestation-policy.json ]; then
        live_version="$(node -e 'try { const v=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).version; console.log(Number.isSafeInteger(v) ? v : 0); } catch { console.log(0); }' "$live")"
        [ "$live_version" -le "$current" ] || { echo 'Live policy is newer than this checkout' >&2; exit 1; }
      fi
      missing=true
      ;;
    404) missing=true ;;
    *)
      echo "Cannot check $host: $status" >&2
      if [ "$host" = https://nymira.pages.dev ] || [ "$missing" = false ]; then exit 1; fi
      ;;
  esac
done
if [ "$missing" = false ]; then echo 'live=true' >> "${GITHUB_OUTPUT:-/dev/null}"; exit 0; fi
cutoff="$(node -e 'console.log(new Date(Date.now() - 30 * 60_000).toISOString())')"
active="$(gh run list --repo "$GITHUB_REPOSITORY" --workflow web-deploy.yml --limit 100 --json status,createdAt --jq "any(.[]; .status == \"in_progress\" or (.status != \"completed\" and .createdAt >= \"$cutoff\"))")"
if [ "$active" = true ]; then echo 'A web deployment is already in progress'; exit 0; fi
sha="$(git rev-parse HEAD)"
remote="$(git ls-remote origin refs/heads/main | cut -f1)"
[ "$sha" = "$remote" ] || { echo 'Main changed during the check; try again on the next run' >&2; exit 1; }
echo "The current policy is not live; starting a deployment for $tag"
if [ "$current" -gt "$bundled" ]; then
  gh workflow run web-deploy.yml --repo "$GITHUB_REPOSITORY" \
    -f ref="$tag" -f profile=testnet -f version="${tag#web-v}" -f policy_only=true -f policy_ref="$sha"
else
  gh workflow run web-deploy.yml --repo "$GITHUB_REPOSITORY" \
    -f ref="$tag" -f profile=testnet -f version="${tag#web-v}"
fi
