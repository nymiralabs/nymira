#!/usr/bin/env bash
# Signs the next attestation policy after NEAR AI redeployed (ADR 0021).
#   scripts/policy-update.sh          you answer two questions; the key is a file on your machine
#   scripts/policy-update.sh --auto   no questions, for the policy-watch workflow: it signs only a
#                                     new compose hash on a machine image the policy already lists, at
#                                     most 3 times in 24 hours, and writes a report to $AUTO_REPORT
# It reads NEAR AI's live reports, shows what the signed policy does not list, and, if you say yes,
# signs the next version with the offline policy key, checks it the way a client does, writes it
# to both pins folders with a changelog line, commits it and (after a second yes) pushes it to main.
# The `policy-deploy` workflow then puts the file on the site beside the current web release.
# This script never reads the key: `policy_tool` does, and it is the only thing that touches it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
KEY="${NYMIRA_POLICY_KEY:-$HOME/nymira-policy.key}"
POLICY=crates/nymirad/pins/attestation-policy.json
WEB_POLICY=web/pins/attestation-policy.json
CHANGELOG=crates/nymirad/pins/attestation-policy.changelog.md
die() { echo "policy-update: $*" >&2; exit 1; }
AUTO=0
[ "${1:-}" = "--auto" ] && AUTO=1

[ -f "$KEY" ] || die "no policy key at $KEY (set NYMIRA_POLICY_KEY)"
[ "$(git config user.email)" = "345380708+nymiralabs@users.noreply.github.com" ] || die "this repository must commit as Nymira Labs"
git diff --quiet -- crates/nymirad/pins web/pins || die "the pins folders have uncommitted changes"
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "check out an up-to-date main first"

draft="$(mktemp)"
trap 'rm -f "$draft"' EXIT
rc=0
report="$(node web/scripts/watch-policy.mjs --json --draft "$draft")" || rc=$?
case "$rc" in
  0) echo "Every live deployment is listed. Nothing to sign."; exit 0 ;;
  3) ;;
  *) die "could not read NEAR AI's reports (exit $rc)" ;;
esac

if [ "$AUTO" = 1 ]; then
  # The read above takes quote fields as they are. What gets signed comes from a second, fresh
  # read in which every entry is verified (Intel quote and TCB, nonce, signer, event log, compose),
  # so an entry that does not verify, or a read that fails, signs nothing.
  verified="$(mktemp)"
  trap 'rm -f "$draft" "$verified"' EXIT
  node --import "$ROOT/web/scripts/lib/register-ts.mjs" "$ROOT/web/scripts/verify-live.mjs" --out "$verified" \
    || { echo "policy-update: the live evidence did not verify; a person must look" >&2; exit 7; }
  rc=0
  report="$(node web/scripts/watch-policy.mjs --observed "$verified" --json --draft "$draft")" || rc=$?
  case "$rc" in
    0) echo "Every verified deployment is listed. Nothing to sign."; exit 0 ;;
    3) ;;
    *) die "could not compare the verified evidence (exit $rc)" ;;
  esac
  other="$(printf '%s' "$report" | node -e 'const r=JSON.parse(require("fs").readFileSync(0,"utf8"));console.log(r.unknown.filter(u=>u.why!=="compose"||u.role!=="model").length)')"
  total="$(printf '%s' "$report" | node -e 'console.log(JSON.parse(require("fs").readFileSync(0,"utf8")).unknown.length)')"
  # A new image or new measurements are signed unattended only when the owner turned that on
  # (testnet: the owner trusts NEAR AI's live evidence for them; see ADR 0021).
  if [ "${POLICY_AUTO_NEW_IMAGES:-0}" != 1 ]; then
    [ "$other" = 0 ] || { echo "policy-update: a deployment other than a new model compose hash is not listed; a person must sign it" >&2; exit 5; }
  fi
  [ "$total" -le 6 ] || { echo "policy-update: $total new hashes at once is too many to sign without a person" >&2; exit 5; }
  recent="$(git log --since='24 hours ago' --grep='^Policy version' --oneline | wc -l | tr -d ' ')"
  [ "$recent" -lt 3 ] || { echo "policy-update: $recent policy versions in the last 24 hours; a person must look" >&2; exit 6; }
fi

echo "The signed policy does not list:"
printf '%s' "$report" | node -e '
  const r = JSON.parse(require("fs").readFileSync(0, "utf8"));
  for (const u of r.unknown) console.log(`  ${u.why === "image" ? "NEW IMAGE  " : "new compose"} ${u.role}  compose ${u.compose_hash}  os ${u.os_image_hash.slice(0, 16)}...`);
'
if printf '%s' "$report" | grep -q '"why": "image"'; then
  echo
  echo "A new IMAGE changes the measurements the policy trusts, not just a compose file."
  echo "Record and check it first: cargo run -p nymirad --features mock --example record_evidence"
fi
echo
echo "Draft against the current policy:"
diff -u "$POLICY" "$draft" | tail -n +3 || true
echo
current="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).version)' "$POLICY")"
next=$((current + 1))
if [ "$AUTO" = 1 ]; then echo "Signing version $next without a question (--auto)."; else
  read -r -p "Sign this as version $next with $KEY? [y/N] " yes
  [ "$yes" = "y" ] || die "not signed"
fi

cargo run -q -p nymirad --example policy_tool -- sign "$KEY" "$draft" "$POLICY" "$next"
cp "$POLICY" "$WEB_POLICY"
cargo run -q -p nymirad --example policy_tool -- verify crates/nymirad/pins/policy-signer.pub "$WEB_POLICY" "$next" >/dev/null

today="$(date -u +%-d) $(date -u +%B) $(date -u +%Y)"
hashes="$(printf '%s' "$report" | node -e 'const r=JSON.parse(require("fs").readFileSync(0,"utf8"));console.log([...new Set(r.unknown.map(u=>"`"+u.compose_hash.slice(0,12)+"…` ("+u.role+")"))].join(", "))')"
{
  echo
  echo "## Version $next"
  echo
  if [ "$AUTO" = 1 ]; then
    echo "- Lists the deployments seen in NEAR AI's live attestation on $today and not listed before: $hashes. Signed unattended from a fresh read in which every entry verified (Intel quote and an UpToDate TCB, nonce and signer binding, event log, compose file); NVIDIA's GPU verdict was not checked. No reference NEAR AI publishes for these values is known: they are accepted on the provider's word."
  else
    echo "- Lists the deployments seen in NEAR AI's live attestation on $today and not listed before: $hashes. The values were read from the live endpoint and not compared with a reference NEAR AI publishes (none is known): trust on first use."
  fi
  echo "- Everything else is as in version $current."
} >> "$CHANGELOG"

git add "$POLICY" "$WEB_POLICY" "$CHANGELOG"
git commit -q -m "Policy version $next: lists the deployments NEAR AI showed on $today"
echo "Committed: $(git log --oneline -1)"
if [ "$AUTO" = 1 ]; then
  if [ -n "${AUTO_REPORT:-}" ]; then
    {
      echo "The \`policy-watch\` workflow signed policy version $next without a person, because NEAR AI's live reports showed deployments it did not list, and a fresh read verified every entry (Intel quote, TCB UpToDate, nonce and signer binding, event log, compose file; not NVIDIA's GPU verdict):"
      echo
      echo "$hashes"
      echo
      echo "The values are accepted on NEAR AI's word: no reference for them exists. The commit is on \`main\`: \`$(git rev-parse --short HEAD)\`. If this looks wrong, revert it, and sign again by hand."
    } > "$AUTO_REPORT"
  fi
else
  read -r -p "Push to main now? The site gets the policy within minutes. [y/N] " yes
  [ "$yes" = "y" ] || { echo "Not pushed. Push when ready: git push origin main"; exit 0; }
fi
git push origin HEAD:main
echo "Pushed. Watch the policy-deploy workflow; the site serves /attestation-policy.json when it is green."
