import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { afterEach, describe, expect, it } from 'vitest';

const dirs: string[] = [];
afterEach(() => dirs.splice(0).forEach((dir) => rmSync(dir, { recursive: true, force: true })));

function runRecovery(live: string, active = false) {
  const dir = mkdtempSync(join(tmpdir(), 'nymira-recovery-'));
  dirs.push(dir);
  const bin = join(dir, 'bin');
  mkdirSync(bin);
  mkdirSync(join(dir, 'web/pins'), { recursive: true });
  const policy = '{"version":8}';
  writeFileSync(join(dir, 'web/pins/attestation-policy.json'), policy);
  writeFileSync(join(dir, 'live'), live);
  const fake = (name: string, body: string) =>
    writeFileSync(join(bin, name), `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  fake(
    'git',
    `case "$1" in tag) echo web-v0.1.8;; show) echo '{"version":5}';; rev-parse|ls-remote) echo 1234567890123456789012345678901234567890;; esac`,
  );
  fake(
    'curl',
    `while [ "$1" != "-o" ]; do shift; done; cp '${join(dir, 'live')}' "$2"; printf 200`,
  );
  fake(
    'gh',
    `case "$*" in *'run list'*) echo '${active ? 'true' : 'false'}';; *'workflow run'*) echo "$*" > '${join(dir, 'dispatch')}';; *) exit 1;; esac`,
  );
  const result = spawnSync('bash', [resolve('scripts/recover-policy.sh')], {
    cwd: dir,
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, GITHUB_REPOSITORY: 'test/repo' },
    encoding: 'utf8',
  });
  let dispatch = '';
  try {
    dispatch = readFileSync(join(dir, 'dispatch'), 'utf8');
  } catch {
    /* no dispatch */
  }
  return { result, dispatch };
}

describe('policy deployment recovery', () => {
  it('dispatches the signed current policy when the live file differs', () => {
    const { result, dispatch } = runRecovery('{"version":7}');
    expect(result.status).toBe(0);
    expect(dispatch).toContain('policy_ref=1234567890123456789012345678901234567890');
    expect(dispatch).toContain('ref=web-v0.1.8');
  });
  it('does not dispatch while a deployment is active or the live file matches', () => {
    for (const run of [runRecovery('{"version":7}', true), runRecovery('{"version":8}')]) {
      expect(run.result.status).toBe(0);
      expect(run.dispatch).toBe('');
    }
  });
  it('refuses to overwrite a newer live policy', () => {
    const { result, dispatch } = runRecovery('{"version":9}');
    expect(result.status).not.toBe(0);
    expect(dispatch).toBe('');
  });
});
