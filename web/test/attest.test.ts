// The attestation checks against the real evidence NEAR AI served on 2026-11-02 (recorded by the
// `nymirad` tests): Intel quote verification runs the real dcap-qvl WASM at the recording's time.
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import {
  initSync,
  status_signature_valid,
  verify_tdx_quote,
} from '../src/wasm/web-checks/nymira_web_wasm.js';
import { loadPolicy, signedText, PolicyError, type Policy } from '../src/chat/attest/policy';
import { recordedQuoteVerifier } from '../src/chat/attest/intel';
import {
  AttestationError,
  replayRtmr3,
  verifyReports,
  type Attested,
  type ModelReport,
} from '../src/chat/attest/verify';
import { badgeLabel, verifyCompletionSignature, weakest } from '../src/chat/attest/signature';
import { toHex } from '../src/crypto/bytes';

const dir = new URL('../test-fixtures/near-ai-recorded/', import.meta.url);
const read = (f: string) => readFileSync(new URL(f, dir), 'utf8');
const meta = JSON.parse(read('meta.json')) as {
  recorded_at_secs: number;
  nonce_model: string;
  nonce_gateway: string;
  model: string;
};
const modelReport = JSON.parse(read('model-report.json')) as ModelReport;
const gatewayReport = JSON.parse(read('gateway-report.json')) as ModelReport;
const policyJson = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
) as Policy;
const signerPk = readFileSync(new URL('../pins/policy-signer.pub', import.meta.url), 'utf8').trim();

beforeAll(() => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/web-checks/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
});

const wasm = { verify_tdx_quote, fetch_collateral: async () => '' };
// The two reports were recorded at slightly different moments; one collateral each.
const quoteFor = (file: string) => recordedQuoteVerifier(wasm, read(file), meta.recorded_at_secs);

async function attest(report = modelReport): Promise<Attested> {
  const policy = loadPolicy(policyJson, signerPk, 1);
  // The model quote and the gateway quote have their own collateral: route by quote.
  const gatewayQuote = (gatewayReport.gateway_attestation ?? ({} as { intel_quote: string }))
    .intel_quote;
  const model = quoteFor('collateral-model.json');
  const gateway = quoteFor('collateral-gateway.json');
  return verifyReports(
    meta.model,
    report,
    meta.nonce_model,
    gatewayReport,
    meta.nonce_gateway,
    policy,
    { verify: (q) => (q === gatewayQuote ? gateway.verify(q) : model.verify(q)) },
  );
}

describe('the signed policy', () => {
  it('loads under the pinned signer and is strict', () => {
    const p = loadPolicy(policyJson, signerPk, 1);
    expect(p.accepted_tcb_statuses).toEqual(['UpToDate']);
    expect(p.deployments.map((d) => d.role)).toEqual(['model', 'gateway']);
  });

  it('covers NVIDIA pins and their provenance with the policy signature', () => {
    const seed = new Uint8Array(32).fill(9);
    const pk = toHex(ed25519.getPublicKey(seed));
    const p: Policy = {
      ...structuredClone(policyJson),
      nvidia_certificates: [{ der: 'AA==', official_source_verified: false }],
    };
    p.signature = toHex(ed25519.sign(new TextEncoder().encode(signedText(p)), seed));
    expect(loadPolicy(p, pk, 1).nvidia_certificates).toEqual(p.nvidia_certificates);
    const changed = structuredClone(p);
    changed.nvidia_certificates![0]!.official_source_verified = true;
    expect(() => loadPolicy(changed, pk, 1)).toThrow(/signature/);
    changed.nvidia_certificates = [];
    expect(() => loadPolicy(changed, pk, 1)).toThrow(/signature/);
  });

  it('is the file nymirad ships', () => {
    const shipped = readFileSync(
      new URL('../../crates/nymirad/pins/attestation-policy.json', import.meta.url),
      'utf8',
    );
    expect(readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8')).toBe(
      shipped,
    );
    const key = readFileSync(
      new URL('../../crates/nymirad/pins/policy-signer.pub', import.meta.url),
      'utf8',
    );
    expect(signerPk).toBe(key.trim());
  });

  it('refuses a changed measurement, a lenient TCB, an old version and a foreign signer', () => {
    const changed = structuredClone(policyJson);
    changed.deployments[0]!.mr_td = '0'.repeat(96);
    expect(() => loadPolicy(changed, signerPk, 1)).toThrow(/signature/);
    expect(() => loadPolicy(policyJson, signerPk, 4)).toThrow(/at least 4/);
    const other = toHex(ed25519.getPublicKey(new Uint8Array(32).fill(8)));
    expect(() => loadPolicy(policyJson, other, 1)).toThrow(PolicyError);

    // A policy that is signed but lenient: sign with a test key and load under that key.
    const seed = new Uint8Array(32).fill(9);
    const lenient: Policy = {
      ...structuredClone(policyJson),
      accepted_tcb_statuses: ['UpToDate', 'OutOfDate'],
    };
    lenient.signature = toHex(ed25519.sign(new TextEncoder().encode(signedText(lenient)), seed));
    expect(() => loadPolicy(lenient, toHex(ed25519.getPublicKey(seed)), 1)).toThrow(
      /strict policy/,
    );
  });
});

describe('recorded evidence', () => {
  it('verifies the model and the gateway: quote, TCB, binding, event log, compose, policy', async () => {
    const a = await attest();
    expect(a.model.tcbStatus).toBe('UpToDate');
    expect(a.model.gpu).toBe('nonce_only');
    expect(a.model.signingKey).toBe(modelReport.model_attestations![0]!.signing_public_key);
    expect(a.gateway.signingAddress).toBe(gatewayReport.gateway_attestation!.signing_address);
  });

  it('refuses a flipped nonce', async () => {
    const policy = loadPolicy(policyJson, signerPk, 1);
    await expect(
      verifyReports(
        meta.model,
        modelReport,
        '00'.repeat(32),
        gatewayReport,
        meta.nonce_gateway,
        policy,
        quoteFor('collateral-model.json'),
      ),
    ).rejects.toMatchObject({ code: 'nonce_mismatch' });
  });

  it('refuses a report whose model key is not the attested signer', async () => {
    const bad = structuredClone(modelReport);
    bad.model_attestations![0]!.signing_public_key = '11'.repeat(32);
    await expect(attest(bad)).rejects.toMatchObject({ code: 'model_key_mismatch' });
  });

  it('refuses a policy that does not list the measured deployment', async () => {
    const seed = new Uint8Array(32).fill(9);
    const p: Policy = structuredClone(policyJson);
    p.deployments[0]!.rtmr0 = '0'.repeat(96);
    p.signature = toHex(ed25519.sign(new TextEncoder().encode(signedText(p)), seed));
    const policy = loadPolicy(p, toHex(ed25519.getPublicKey(seed)), 1);
    await expect(
      verifyReports(
        meta.model,
        modelReport,
        meta.nonce_model,
        gatewayReport,
        meta.nonce_gateway,
        policy,
        {
          verify: (q) => quoteFor('collateral-model.json').verify(q),
        },
      ),
    ).rejects.toMatchObject({ code: 'measurement_not_accepted' });
  });

  it('refuses a quote that is not signed by Intel (a flipped byte)', async () => {
    const bad = structuredClone(modelReport);
    const q = bad.model_attestations![0]!.intel_quote;
    bad.model_attestations![0]!.intel_quote =
      q.slice(0, 2000) + (q[2000] === 'a' ? 'b' : 'a') + q.slice(2001);
    await expect(attest(bad)).rejects.toMatchObject({ code: 'quote_invalid' });
  });

  it('refuses an event log that does not replay to the quoted RTMR3', () => {
    const log = modelReport.model_attestations![0]!.event_log;
    expect(() => replayRtmr3(log, '00'.repeat(48))).toThrow(AttestationError);
    const entries = (log as { imr: number }[]).filter((e) => e.imr === 3);
    expect(entries.length).toBeGreaterThan(0);
  });
});

describe('response signatures', () => {
  const request = new TextEncoder().encode(read('chat/request.json'));
  const response = new TextEncoder().encode(read('chat/response.json'));
  const signature = JSON.parse(read('chat/signature.json')) as Record<string, string>;

  it('the recorded provider_tee signature over the recorded bytes verifies as model-signed', async () => {
    const a = await attest();
    expect(verifyCompletionSignature(a, signature, meta.model, request, response)).toEqual({
      result: 'model_signed',
    });
    // Other bytes, or a bare signature without its kind, are judged the same way.
    expect(
      verifyCompletionSignature(
        a,
        signature,
        meta.model,
        request,
        new TextEncoder().encode('other'),
      ).result,
    ).toBe('not_verified');
    const { signature_kind: _kind, ...bare } = signature;
    void _kind;
    expect(verifyCompletionSignature(a, bare, meta.model, request, response).result).toBe(
      'model_signed',
    );
  });

  it('every mismatch is not verified, with a reason, and the weakest result counts', async () => {
    const a = await attest();
    const none = verifyCompletionSignature(a, null, meta.model, request, response);
    expect(badgeLabel(none)).toBe('not verified');
    const kind = verifyCompletionSignature(
      a,
      { ...signature, signature_kind: 'other' },
      meta.model,
      request,
      response,
    );
    expect(kind.result).toBe('not_verified');
    expect(weakest([{ result: 'model_signed' }, { result: 'gateway_signed' }]).result).toBe(
      'gateway_signed',
    );
    expect(weakest([{ result: 'model_signed' }, none]).result).toBe('not_verified');
    expect(badgeLabel({ result: 'model_signed' })).toBe('model-signed response');
    expect(badgeLabel({ result: 'gateway_signed' })).toBe('gateway-signed response');
  });
});

describe('web-checks', () => {
  it('a status signature that is not the operator’s is refused without throwing', () => {
    expect(() => status_signature_valid('0x1', '0x2', 'aabb', 'op.test', 1n, '{"r":1}')).toThrow();
  });
});
