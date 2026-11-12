// Fresh, verified evidence of NEAR AI's deployments for the policy signer (ADR 0021, testnet).
// Each report is fetched under a nonce of its own and every entry goes through the checks of the
// app (`authenticateEvidence`): the Intel quote against Intel's root with current collateral, an
// UpToDate TCB, debug off, the nonce and the signer bound in the quote's report data, the RTMR3
// replay, the compose file against `mr_config_id`, and for a model the key and the GPU payload's
// nonce. It does NOT verify NVIDIA's GPU verdict and it does not say the software is safe: it
// says a genuine enclave, answering this request, runs the measured deployment.
import { randomBytes } from 'node:crypto';
import { authenticateEvidence } from '../../src/chat/attest/verify.ts';

/** The only origin the signing path asks. Tests pass their own `base` to `verifyLive`. */
export const NEAR_AI = 'https://cloud-api.near.ai';

/**
 * Every deployment `models` and the gateway show, each fully authenticated, as the tuples a
 * policy lists. Throws on the first entry that fails: one bad entry rejects them all.
 */
export async function verifyLive({
  base,
  models,
  quote,
  get,
  nonce = () => randomBytes(32).toString('hex'),
}) {
  const out = [];
  const skipped = [];
  const add = (role, evidence, a) => {
    const v = a.verified;
    const key = `${role}|${v.composeHash}|${v.osImageHash}|${a.measured.mr_td}`;
    if (out.some((o) => o.key === key)) return;
    out.push({
      key,
      role,
      compose_hash: v.composeHash,
      os_image_hash: v.osImageHash,
      ...a.measured,
    });
  };
  const gnonce = nonce();
  const gateway = await get(base, '/v1/attestation/report', {
    nonce: gnonce,
    signing_algo: 'ed25519',
    include_tls_fingerprint: 'false',
  });
  if (!gateway?.gateway_attestation) throw new Error('no gateway attestation evidence');
  add(
    'gateway',
    gateway.gateway_attestation,
    await authenticateEvidence('gateway', gateway.gateway_attestation, gnonce, quote),
  );
  for (const model of models) {
    const n = nonce();
    let entries;
    try {
      const report = await get(
        base,
        '/v1/attestation/report',
        {
          model,
          provider: 'near',
          nonce: n,
          include_tls_fingerprint: 'false',
          signing_algo: 'ed25519',
        },
        { 'x-no-aliasing': 'true' },
      );
      entries = (report?.model_attestations ?? []).filter(
        (e) => e.model_name === undefined || e.model_name === model,
      );
      if (entries.length === 0) throw new Error(`no attestation evidence for model ${model}`);
    } catch (e) {
      // A report that cannot be read holds back that model only: nothing of it is signed.
      skipped.push(`${model}: ${e instanceof Error ? e.message : e}`);
      continue;
    }
    // Evidence that was read and does not verify rejects everything: an entry that lies is not a
    // network fault.
    for (const e of entries) {
      add('model', e, await authenticateEvidence('model', e, n, quote));
    }
  }
  if (out.every((o) => o.role !== 'model')) {
    throw new Error(`no model evidence could be read: ${skipped.join('; ')}`);
  }
  out.skipped = skipped;
  return out;
}

/** A GET that refuses redirects, so the origin that answers is the one that was asked. */
export async function fixedGet(base, path, query, headers = {}) {
  const res = await fetch(`${base}${path}?${new URLSearchParams(query)}`, {
    headers,
    redirect: 'error',
    signal: AbortSignal.timeout(30_000),
  });
  if (!res.ok) throw new Error(`${path}: ${res.status}`);
  return res.json();
}
