// Asks NEAR AI's live attestation endpoints what the model and gateway deployments measure and
// says whether the signed policy lists them (ADR 0021). Exit codes: 0 all listed, 3 something is
// not listed (a draft goes to --draft), 4 the endpoints could not be read (not a finding).
//   node scripts/watch-policy.mjs [--policy FILE] [--base URL] [--models a,b] [--draft OUT] [--json]
//                                [--observed FILE]
// It verifies no quote. A person records the evidence with `record_evidence`, checks it and signs.
// --observed FILE reads the deployments from the output of `verify-live.mjs` instead of the network:
// the policy signer's job drafts only from evidence that script has verified.
import { randomBytes } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { draftFrom, observeReports, unknownDeployments } from './lib/policy-watch.mjs';

const arg = (name, fallback) => {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : fallback;
};
const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const policyFile = arg('--policy', here('../../crates/nymirad/pins/attestation-policy.json'));
const base = arg('--base', 'https://cloud-api.near.ai');
const modelsFile = readFileSync(here('../src/chat/models.ts'), 'utf8');
const models = (
  arg('--models') ?? [...modelsFile.matchAll(/id: '([^']+)'/g)].map((m) => m[1]).join(',')
).split(',');
const policy = JSON.parse(readFileSync(policyFile, 'utf8'));
// A test hook for the signing path: compose hashes that start with these prefixes are treated as
// missing from the policy, so the watcher finds them "new". The draft can only add what NEAR AI
// serves live, so the hook cannot weaken a policy.
for (const prefix of (process.env.POLICY_WATCH_PRETEND_MISSING ?? '').split(',').filter(Boolean)) {
  for (const d of policy.deployments) {
    if (d.role === 'model')
      d.compose_hashes = d.compose_hashes.filter((h) => !h.startsWith(prefix));
  }
}

const get = async (path, query, headers = {}) => {
  const res = await fetch(`${base}${path}?${new URLSearchParams(query)}`, {
    headers,
    signal: AbortSignal.timeout(30_000),
  });
  if (!res.ok) throw new Error(`${path}: ${res.status}`);
  return res.json();
};

let observed = [];
const observedFile = arg('--observed');
try {
  if (observedFile) {
    observed = JSON.parse(readFileSync(observedFile, 'utf8'));
    if (!Array.isArray(observed) || observed.some((o) => !o?.role || !o?.compose_hash))
      throw new Error(`${observedFile} is not a list of verified deployments`);
  } else {
    const nonce = () => randomBytes(32).toString('hex');
    const gateway = await get('/v1/attestation/report', {
      nonce: nonce(),
      signing_algo: 'ed25519',
      include_tls_fingerprint: 'false',
    });
    for (const model of models) {
      const report = await get(
        '/v1/attestation/report',
        {
          model,
          provider: 'near',
          nonce: nonce(),
          include_tls_fingerprint: 'false',
          signing_algo: 'ed25519',
        },
        { 'x-no-aliasing': 'true' },
      );
      observed.push(...observeReports(model, report, gateway));
    }
  }
} catch (e) {
  console.error(`could not read the attestation reports: ${e instanceof Error ? e.message : e}`);
  process.exit(4);
}

const unknown = unknownDeployments(policy, observed);
if (process.argv.includes('--json')) {
  console.log(JSON.stringify({ policy_version: policy.version, models, unknown }, null, 2));
} else {
  console.log(
    `policy version ${policy.version}; ${observed.length} deployments read for ${models.length} models`,
  );
  for (const u of unknown) {
    console.log(
      `NOT LISTED (${u.why}): ${u.role} compose ${u.compose_hash} os ${u.os_image_hash.slice(0, 16)}…`,
    );
  }
  if (unknown.length === 0) console.log('every deployment is listed');
}
const out = arg('--draft');
if (out && unknown.length > 0)
  writeFileSync(out, JSON.stringify(draftFrom(policy, unknown), null, 2));
process.exit(unknown.length > 0 ? 3 : 0);
