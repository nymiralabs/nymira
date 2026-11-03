import { describe, expect, it } from 'vitest';
import { checkPackages, lockedPackages } from '../scripts/lib/deps.mjs';

const NOW = Date.parse('2026-11-10T00:00:00Z');
const day = (n: number) => new Date(NOW - n * 86_400_000).toISOString();

const registry: Record<string, unknown> = {
  old: {
    versions: { '1.0.0': { dist: { integrity: 'sha512-a', attestations: { provenance: {} } } } },
    time: { '1.0.0': day(30) },
  },
  young: {
    versions: { '2.0.0': { dist: { integrity: 'sha512-b', attestations: { provenance: {} } } } },
    time: { '2.0.0': day(2) },
  },
  bare: {
    versions: { '1.0.0': { dist: { integrity: 'sha512-c' } } },
    time: { '1.0.0': day(60) },
  },
};
const packument = async (name: string) => registry[name] as never;
const pkg = (name: string, version: string, integrity: string) => ({ name, version, integrity });

describe('dependency check', () => {
  it('reads registry packages from a lockfile and flags the others', () => {
    const lock = {
      packages: {
        '': {},
        'node_modules/old': {
          version: '1.0.0',
          resolved: 'https://registry.npmjs.org/old/-/old-1.0.0.tgz',
          integrity: 'x',
        },
        'node_modules/a/node_modules/@s/b': {
          version: '1.0.0',
          resolved: 'https://evil.example/b.tgz',
          integrity: 'y',
        },
      },
    };
    const found = lockedPackages(lock);
    expect(found.map((p: { name: string }) => p.name)).toEqual(['old', '@s/b']);
    expect(found[1]?.problem).toMatch(/not from the npm registry/);
  });

  it('passes an old package with provenance', async () => {
    const r = await checkPackages([pkg('old', '1.0.0', 'sha512-a')], packument, {}, NOW);
    expect(r).toEqual({ problems: [], noProvenance: [], tooYoung: [] });
  });

  it('fails a package younger than the minimum age, unless excepted, and flags a stale exception', async () => {
    const young = [pkg('young', '2.0.0', 'sha512-b')];
    expect((await checkPackages(young, packument, {}, NOW)).problems).toEqual([
      'young@2.0.0: younger than 7 days',
    ]);
    const ok = await checkPackages(young, packument, { age: { 'young@2.0.0': 'why' } }, NOW);
    expect(ok.problems).toEqual([]);
    const later = await checkPackages(
      young,
      packument,
      { age: { 'young@2.0.0': 'why' } },
      NOW + 10 * 86_400_000,
    );
    expect(later.problems).toEqual(['young@2.0.0: age exception no longer needed; remove it']);
  });

  it('fails a lockfile hash that is not the registry’s', async () => {
    const r = await checkPackages([pkg('old', '1.0.0', 'sha512-tampered')], packument, {}, NOW);
    expect(r.problems).toEqual(["old@1.0.0: lockfile integrity differs from the registry's"]);
  });

  it('asks for a reason when a version has no provenance', async () => {
    const bare = [pkg('bare', '1.0.0', 'sha512-c')];
    expect((await checkPackages(bare, packument, {}, NOW)).noProvenance).toEqual(['bare@1.0.0']);
    const ok = await checkPackages(
      bare,
      packument,
      { provenance: { 'bare@1.0.0': 'none published' } },
      NOW,
    );
    expect(ok.noProvenance).toEqual([]);
    const stale = await checkPackages([], packument, { provenance: { 'bare@1.0.0': 'x' } }, NOW);
    expect(stale.problems).toHaveLength(1);
  });
});
