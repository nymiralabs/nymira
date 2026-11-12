// The policy signer's evidence check (scripts/lib/verify-live.mjs) against the real evidence NEAR AI
// served on 2026-11-02, and against altered copies of it.
import { readFileSync } from 'node:fs';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { initSync, verify_tdx_quote } from '../src/wasm/web-checks/nymira_web_wasm.js';
import { recordedQuoteVerifier } from '../src/chat/attest/intel';
import type { ModelReport } from '../src/chat/attest/verify';
import { fixedGet, verifyLive } from '../scripts/lib/verify-live.mjs';
import { unknownDeployments } from '../scripts/lib/policy-watch.mjs';

const dir = new URL('../test-fixtures/near-ai-recorded/', import.meta.url);
const read = (f: string) => readFileSync(new URL(f, dir), 'utf8');
const meta = JSON.parse(read('meta.json')) as {
  recorded_at_secs: number;
  nonce_model: string;
  nonce_gateway: string;
  model: string;
};
const policy = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
);

beforeAll(() => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/web-checks/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
});

const wasm = { verify_tdx_quote, fetch_collateral: async () => '' };
const gatewayReport = JSON.parse(read('gateway-report.json')) as ModelReport;
const modelReport = () => JSON.parse(read('model-report.json')) as ModelReport;
const gatewayQuote = gatewayReport.gateway_attestation!.intel_quote;
const quote = (() => {
  const model = recordedQuoteVerifier(wasm, read('collateral-model.json'), meta.recorded_at_secs);
  const gateway = recordedQuoteVerifier(
    wasm,
    read('collateral-gateway.json'),
    meta.recorded_at_secs,
  );
  return { verify: (q: string) => (q === gatewayQuote ? gateway.verify(q) : model.verify(q)) };
})();

/** Serves the recorded reports; the gateway report is asked first, then each model's. */
function run(model: ModelReport, nonces = [meta.nonce_gateway, meta.nonce_model]) {
  let n = 0;
  return verifyLive({
    base: 'https://example.invalid',
    models: [meta.model],
    quote,
    nonce: () => nonces[n++] ?? nonces[nonces.length - 1]!,
    get: async (_b: string, _p: string, query: Record<string, string>) =>
      query.model ? model : gatewayReport,
  });
}

describe('verifyLive', () => {
  it('returns the deployments of verified evidence, and the signed policy lists them', async () => {
    const got = await run(modelReport());
    expect(got.map((d: { role: string }) => d.role).sort()).toEqual(['gateway', 'model']);
    expect(unknownDeployments(policy, got)).toEqual([]);
  });

  it('refuses a report that answers another nonce', async () => {
    await expect(run(modelReport(), [meta.nonce_gateway, 'ab'.repeat(32)])).rejects.toThrow(
      /nonce/,
    );
  });

  it('refuses a compose file that is not the one the quote binds', async () => {
    const r = modelReport();
    const entry = r.model_attestations![0]!;
    entry.info = { tcb_info: { app_compose: `${entry.info!.tcb_info!.app_compose!} ` } };
    await expect(run(r)).rejects.toThrow(/compose/);
  });

  it('refuses an event log that does not replay to the quoted RTMR3', async () => {
    const r = modelReport();
    const log = JSON.parse(JSON.stringify(r.model_attestations![0]!.event_log)) as {
      event?: string;
      event_payload: string;
    }[];
    const e = log.find((x) => x.event === 'os-image-hash')!;
    e.event_payload = `${e.event_payload.slice(0, -1)}${e.event_payload.endsWith('0') ? '1' : '0'}`;
    r.model_attestations![0]!.event_log = log;
    await expect(run(r)).rejects.toThrow();
  });

  it('refuses a quote that does not verify', async () => {
    const r = modelReport();
    const q = r.model_attestations![0]!.intel_quote;
    r.model_attestations![0]!.intel_quote = `${q.slice(0, 2000)}${q[2000] === '0' ? '1' : '0'}${q.slice(2001)}`;
    await expect(run(r)).rejects.toThrow();
  });

  it('refuses everything when one entry fails, even if another verified', async () => {
    const r = modelReport();
    const bad = JSON.parse(JSON.stringify(r.model_attestations![0]!));
    bad.info.tcb_info.app_compose += ' ';
    r.model_attestations!.push(bad);
    await expect(run(r)).rejects.toThrow(/compose/);
  });

  it('refuses a report without evidence for the model or the gateway', async () => {
    await expect(run({ model_attestations: [] })).rejects.toThrow(/no attestation evidence/);
  });
});

describe('fixedGet', () => {
  afterEach(() => vi.restoreAllMocks());

  it('does not follow a redirect', async () => {
    const spy = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{}'));
    await fixedGet('https://cloud-api.near.ai', '/v1/attestation/report', { nonce: 'x' });
    expect(spy.mock.calls[0]![1]).toMatchObject({ redirect: 'error' });
  });
});
