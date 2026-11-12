import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { afterEach, describe, expect, it } from 'vitest';

const dirs: string[] = [];
afterEach(() => dirs.splice(0).forEach((dir) => rmSync(dir, { recursive: true, force: true })));

function check(main: number, bundled: number, side?: number, live?: number) {
  const dir = mkdtempSync(join(tmpdir(), 'nymira-deploy-'));
  dirs.push(dir);
  const site = join(dir, 'site');
  mkdirSync(site);
  const current = join(dir, 'current.json');
  const release = join(dir, 'release.json');
  writeFileSync(current, JSON.stringify({ version: main }));
  writeFileSync(release, JSON.stringify({ version: bundled }));
  if (side !== undefined)
    writeFileSync(join(site, 'attestation-policy.json'), JSON.stringify({ version: side }));
  const args = [resolve('scripts/check-deploy-policy.mjs'), current, release, site];
  if (live !== undefined)
    args.push(`data:application/json,${encodeURIComponent(JSON.stringify({ version: live }))}`);
  return spawnSync('node', args);
}

describe('deployment policy freshness', () => {
  it('refuses a site with an older side or bundled policy than main', () => {
    expect(check(10, 8, 9).status).not.toBe(0);
    expect(check(10, 9).status).not.toBe(0);
  });
  it('accepts the current policy beside or inside a release', () => {
    expect(check(10, 8, 10).status).toBe(0);
    expect(check(10, 10).status).toBe(0);
  });
  it('refuses to replace a newer policy already served by this host', () => {
    expect(check(9, 9, undefined, 10).status).not.toBe(0);
    expect(check(9, 9, undefined, 9).status).toBe(0);
  });
  it('replaces a live file that is not a policy, and stays on the safe side for an error status', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'nymira-unreadable-'));
    dirs.push(dir);
    const site = join(dir, 'site');
    mkdirSync(site);
    const file = join(dir, 'policy.json');
    writeFileSync(file, '{"version":9}');
    const run = (url: string) =>
      spawnSync('node', [resolve('scripts/check-deploy-policy.mjs'), file, file, site, url]);
    // 200 with a body that is not a policy: the deployment goes ahead.
    expect(run('data:application/json,invalid').status).toBe(0);
    expect(run(`data:application/json,${encodeURIComponent('{"version":"x"}')}`).status).toBe(0);
    // An error status: the live version is unknown, so it stops.
    const server = createServer((_req, res) => {
      res.statusCode = 500;
      res.end('oops');
    });
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
    const { port } = server.address() as { port: number };
    const failed = await new Promise<number | null>((done) => {
      const child = spawn('node', [
        resolve('scripts/check-deploy-policy.mjs'),
        file,
        file,
        site,
        `http://127.0.0.1:${port}/`,
      ]);
      child.on('close', done);
    });
    server.close();
    expect(failed).not.toBe(0);
  });
});
