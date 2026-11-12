// The policy watcher (ADR 0021): what a report measures, and what the shipped policy lists.
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  draftFrom,
  observeReports,
  parseQuote,
  unknownDeployments,
} from '../scripts/lib/policy-watch.mjs';

const fixture = (name: string) =>
  JSON.parse(
    readFileSync(
      new URL(`../../crates/nymirad/tests/fixtures/near-ai-recorded/${name}`, import.meta.url),
      'utf8',
    ),
  );
const policy = JSON.parse(
  readFileSync(new URL('../pins/attestation-policy.json', import.meta.url), 'utf8'),
);
const modelReport = fixture('model-report.json');
const gatewayReport = fixture('gateway-report.json');
const model = modelReport.model_attestations[0].model_name as string;
const observed = observeReports(model, modelReport, gatewayReport);
type Seen = { role: string; compose_hash: string; os_image_hash: string };
const of = (list: Seen[], role: string) => list.find((o) => o.role === role)!;

describe('the policy watcher', () => {
  it('reads the same measurements from a quote that the signed policy lists', () => {
    const q = parseQuote(modelReport.model_attestations[0].intel_quote);
    const listed = policy.deployments.find((d: { role: string }) => d.role === 'model');
    expect([q.mr_td, q.rtmr0, q.rtmr1, q.rtmr2]).toEqual([
      listed.mr_td,
      listed.rtmr0,
      listed.rtmr1,
      listed.rtmr2,
    ]);
  });

  it('finds a model and a gateway deployment in the recorded reports and lists both', () => {
    expect(observed.map((o: { role: string }) => o.role).sort()).toEqual(['gateway', 'model']);
    expect(unknownDeployments(policy, observed)).toEqual([]);
  });

  it('fails when a report contains no model or gateway evidence', () => {
    expect(() => observeReports(model, { model_attestations: [] }, gatewayReport)).toThrow();
    expect(() => observeReports(model, modelReport, {})).toThrow();
  });

  it('flags a model deployment whose compose hash the policy lacks, and drafts the next version', () => {
    const changed: Seen[] = structuredClone(observed);
    of(changed, 'model').compose_hash = 'ab'.repeat(32);
    const unknown = unknownDeployments(policy, changed);
    expect(unknown).toHaveLength(1);
    expect(unknown[0]).toMatchObject({ role: 'model', why: 'compose' });
    const draft = draftFrom(policy, unknown);
    expect(draft.version).toBe(policy.version + 1);
    expect(draft.signature).toBe('');
    const entry = draft.deployments.find((d: { role: string }) => d.role === 'model');
    expect(entry.compose_hashes).toContain('ab'.repeat(32));
    expect(entry.compose_hashes.length).toBe(
      policy.deployments.find((d: { role: string }) => d.role === 'model').compose_hashes.length +
        1,
    );
  });

  it('flags a new machine image as `image` and gives it its own entry in the draft', () => {
    const changed: Seen[] = structuredClone(observed);
    of(changed, 'model').os_image_hash = 'cd'.repeat(32);
    const unknown = unknownDeployments(policy, changed);
    expect(unknown[0]).toMatchObject({ why: 'image' });
    const draft = draftFrom(policy, unknown);
    expect(draft.deployments.length).toBe(policy.deployments.length + 1);
  });

  it('does not flag a gateway compose hash: that entry accepts any, and keeps its pins', () => {
    const changed: Seen[] = structuredClone(observed);
    of(changed, 'gateway').compose_hash = 'ef'.repeat(32);
    expect(unknownDeployments(policy, changed)).toEqual([]);
    of(changed, 'gateway').os_image_hash = '12'.repeat(32);
    expect(unknownDeployments(policy, changed)[0]).toMatchObject({ role: 'gateway', why: 'image' });
  });

  it('refuses a report that is not a TDX v4 quote', () => {
    expect(() => parseQuote('00'.repeat(700))).toThrow(/not a TDX v4 quote/);
  });
});
