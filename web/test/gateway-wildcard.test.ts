// A `gateway` deployment whose `compose_hashes` is the single entry "*" accepts any compose hash;
// everything else about it stays pinned. The same rules as `nymirad` (`Policy::load`,
// `Deployment::accepts_compose`), against the evidence NEAR AI served on 2026-11-02.
import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519.js';
import { initSync, verify_tdx_quote } from '../src/wasm/web-checks/nymira_web_wasm.js';
import { loadPolicy, signedText, type Policy } from '../src/chat/attest/policy';
import { recordedQuoteVerifier } from '../src/chat/attest/intel';
import { verifyReports, type ModelReport } from '../src/chat/attest/verify';
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
const shipped = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
) as Policy;

beforeAll(() => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/web-checks/nymira_web_wasm_bg.wasm', import.meta.url),
    ),
  });
});

const wasm = { verify_tdx_quote, fetch_collateral: async () => '' };
const quoteFor = (file: string) => recordedQuoteVerifier(wasm, read(file), meta.recorded_at_secs);

const seed = new Uint8Array(32).fill(9);
const testPk = toHex(ed25519.getPublicKey(seed));
const signed = (p: Policy): Policy => ({
  ...p,
  signature: toHex(ed25519.sign(new TextEncoder().encode(signedText(p)), seed)),
});

/** The shipped policy with the gateway's compose hashes replaced, signed with a test key. */
function withGateway(
  compose: string[],
  edit: (d: Policy['deployments'][number]) => void = () => {},
): Policy {
  const p = structuredClone(shipped);
  const gateway = p.deployments.find((d) => d.role === 'gateway')!;
  gateway.compose_hashes = compose;
  edit(gateway);
  return signed(p);
}

async function attest(policyJson: Policy) {
  const policy = loadPolicy(policyJson, testPk, 1);
  const gatewayQuote = gatewayReport.gateway_attestation!.intel_quote;
  const model = quoteFor('collateral-model.json');
  const gateway = quoteFor('collateral-gateway.json');
  return verifyReports(
    meta.model,
    modelReport,
    meta.nonce_model,
    gatewayReport,
    meta.nonce_gateway,
    policy,
    { verify: (q) => (q === gatewayQuote ? gateway.verify(q) : model.verify(q)) },
  );
}

describe('a gateway deployment that accepts any compose hash', () => {
  it('verifies the recorded reports although no compose hash is listed', async () => {
    const a = await attest(withGateway(['*']));
    expect(a.gateway.signingAddress).toBe(gatewayReport.gateway_attestation!.signing_address);
    expect(a.model.tcbStatus).toBe('UpToDate');
  });

  it('still refuses another OS image or a changed measurement', async () => {
    await expect(
      attest(withGateway(['*'], (d) => (d.os_image_hash = '00'.repeat(32)))),
    ).rejects.toMatchObject({ code: 'deployment_rejected' });
    for (const field of ['mr_td', 'rtmr0', 'rtmr1', 'rtmr2'] as const) {
      await expect(
        attest(withGateway(['*'], (d) => (d[field] = '00'.repeat(48)))),
      ).rejects.toMatchObject({ code: 'measurement_not_accepted' });
    }
  });

  it('keeps the quote and nonce checks', async () => {
    const policy = loadPolicy(withGateway(['*']), testPk, 1);
    await expect(
      verifyReports(
        meta.model,
        modelReport,
        meta.nonce_model,
        gatewayReport,
        '00'.repeat(32),
        policy,
        {
          verify: (q) =>
            (q === gatewayReport.gateway_attestation!.intel_quote
              ? quoteFor('collateral-gateway.json')
              : quoteFor('collateral-model.json')
            ).verify(q),
        },
      ),
    ).rejects.toMatchObject({ code: 'nonce_mismatch' });
  });

  it('is signed as it is: the text carries the sentinel', () => {
    expect(signedText(withGateway(['*']))).toContain(',*,');
  });

  it('is refused on the model, and mixed with hashes, and as junk', () => {
    const onModel = structuredClone(shipped);
    onModel.deployments.find((d) => d.role === 'model')!.compose_hashes = ['*'];
    expect(() => loadPolicy(signed(onModel), testPk, 1)).toThrow(/malformed/);
    for (const mixed of [['*', 'ab'.repeat(32)], ['ab'.repeat(32), '*'], ['*', '*'], ['**']]) {
      expect(() => loadPolicy(withGateway(mixed), testPk, 1)).toThrow(/malformed/);
    }
  });
});

describe('a gateway deployment with real hashes', () => {
  it('still refuses a compose hash that is not listed', async () => {
    await expect(attest(withGateway(['11'.repeat(32)]))).rejects.toMatchObject({
      code: 'deployment_rejected',
    });
  });

  it('still accepts the recorded one (the shipped policy)', async () => {
    const a = await attest(signed(structuredClone(shipped)));
    expect(a.gateway.signingAddress).toBeTruthy();
  });
});
