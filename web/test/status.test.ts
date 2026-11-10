import { describe, expect, it } from 'vitest';
import {
  age,
  gradeOperator,
  gradeRelease,
  gradeRpc,
  gradeVault,
  overallOf,
} from '../src/status/live';

const healthy = {
  heartbeat_age_secs: 1283,
  open_notes: 7,
  external_notes: 7,
  watchers: [{ id: 'laptop', last_poll_ts: 1000 }],
  degraded: false,
  recovery_mode: false,
  replica: 'ok',
  emergency_notice: null,
  admission: { load_pct: 0 },
  daily_budget: { limit: '50000', used: '0' },
};

describe('status grading', () => {
  it('a healthy operator is ok', () => {
    expect(gradeOperator(healthy, 1010).level).toBe('ok');
  });
  it('a stale heartbeat, a degraded flag or a notice is a warning, never silent', () => {
    expect(gradeOperator({ ...healthy, heartbeat_age_secs: 4000 }, 1010).level).toBe('warn');
    expect(gradeOperator({ ...healthy, degraded: true }, 1010).level).toBe('warn');
    const noticed = gradeOperator({ ...healthy, emergency_notice: 'maintenance' }, 1010);
    expect(noticed.level).toBe('warn');
    expect(noticed.summary).toContain('maintenance');
    expect(gradeOperator({ ...healthy, replica: 'behind' }, 1010).summary).toContain('behind');
  });
  it('a vault mismatch is a failure and names what differs', () => {
    const c = gradeVault({ status: 'mismatch', diffs: ['config: note_fee'] }, 'v.testnet');
    expect(c.level).toBe('fail');
    expect(c.summary).toContain('config: note_fee');
  });
  it('an unpinned or unreadable vault is a warning, a matching one is ok', () => {
    expect(gradeVault({ status: 'unpinned' }, 'v').level).toBe('warn');
    expect(gradeVault({ status: 'unreachable', detail: 'x' }, 'v').level).toBe('warn');
    const ok = gradeVault(
      { status: 'ok', checkedAt: 1, limits: { tvlCap: 100_000_000n, currentTvl: 5_000_000n } },
      'v',
    );
    expect(ok.level).toBe('ok');
    expect(ok.facts).toContainEqual(['Open deposits', '5.00 USDC']);
  });
  it('rpc: all up is ok, some up is a warning, none up is a failure', () => {
    const up = { ok: true, ms: 100, height: 5, blockAgeSecs: 2 };
    const down = { ok: false, ms: 6000, error: 'timeout' };
    expect(gradeRpc([{ url: 'https://a.example', ...up }]).level).toBe('ok');
    expect(
      gradeRpc([
        { url: 'https://a.example', ...up },
        { url: 'https://b.example', ...down },
      ]).level,
    ).toBe('warn');
    expect(gradeRpc([{ url: 'https://b.example', ...down }]).level).toBe('fail');
    expect(gradeRpc([{ url: 'https://a.example', ...up, blockAgeSecs: 500 }]).level).toBe('warn');
  });
  it('a missing manifest is a warning', () => {
    expect(gradeRelease(null).level).toBe('warn');
    expect(gradeRelease({ version: '0.1.3', bundle_hash: 'ab'.repeat(32) }).level).toBe('ok');
  });
  it('the overall level is the worst one', () => {
    expect(overallOf([{ level: 'ok' }, { level: 'warn' }]).overall).toBe('warn');
    expect(overallOf([{ level: 'warn' }, { level: 'fail' }]).overall).toBe('fail');
    expect(overallOf([{ level: 'ok' }]).overall).toBe('ok');
  });
  it('ages read plainly', () => {
    expect(age(30)).toBe('30 s');
    expect(age(1283)).toBe('21 min');
    expect(age(7200)).toBe('2.0 h');
    expect(age(-1)).toBe('unknown');
  });
});
