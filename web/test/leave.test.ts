// The expiry guard's rules against the same numbers `nymirad` tests (`expiry.rs`), the close
// screen's fixed wording, and the operator client's close routes.
import { describe, expect, it } from 'vitest';
import {
  DAY,
  RETRY_SECS,
  DEFAULT_SETTINGS,
  due,
  leaseBlocked,
  noticeLevel,
  noticeText,
  plan,
} from '../src/leave/guard';
import { CLOSE_REVEALS, OLD_STATE_LIMIT, PUBLIC_SENTENCE, plain } from '../src/leave/text';
import { OperatorClient, OperatorError, describeOperatorError } from '../src/chat/operator';
import { deadlineSecs, exitRequest } from '../src/chain/leave';
import { RpcClient } from '../src/chain/rpc';
import type { NoteRecord } from '../src/store/types';

const NOW = 1_800_000_000;

const note = (patch: Partial<NoteRecord> & { expiry: number }): NoteRecord =>
  ({
    id: 'n1',
    network: 'testnet',
    vault: 'vault.test',
    token: 'usdc.test',
    denomination: '1000000',
    payout: 'alice.test',
    payoutExplicit: false,
    sender: 'alice.test',
    commitment: '0xaa',
    phase: 'accepted',
    noteId: 7,
    createdAt: 0,
    updatedAt: 0,
    kind: 'state',
    sealed: { iv: '', ct: '' },
    ...patch,
  }) as NoteRecord;

describe('expiry cut-offs', () => {
  it('are five and three days, the same as nymirad', () => {
    const at = (secs: number) => NOW + secs;
    expect(leaseBlocked(at(5 * DAY + 1), NOW)).toBe(false);
    expect(leaseBlocked(at(5 * DAY), NOW)).toBe(true);
    expect(due(at(5 * DAY + 1), NOW)).toBe('nothing');
    expect(due(at(5 * DAY), NOW)).toBe('close');
    expect(due(at(3 * DAY + 1), NOW)).toBe('close');
    expect(due(at(3 * DAY), NOW)).toBe('exit');
    expect(due(NOW - 10, NOW)).toBe('exit');
    expect(leaseBlocked(NOW - 10, NOW)).toBe(true);
  });

  it('give notices at 14, 7 and 2 days', () => {
    const level = (secs: number) => noticeLevel(NOW + secs, NOW);
    expect(level(14 * DAY + 1)).toBeUndefined();
    expect(level(14 * DAY)).toBe(14);
    expect(level(8 * DAY)).toBe(14);
    expect(level(7 * DAY)).toBe(7);
    expect(level(2 * DAY + 1)).toBe(7);
    expect(level(2 * DAY)).toBe(2);
    expect(level(0)).toBe(2);
    expect(noticeText(7, 7, 6 * DAY)).toContain('note #7 expires in 6 days (7-day notice)');
  });
});

describe('guard plan', () => {
  const on = DEFAULT_SETTINGS;

  it('is on by default and closing at session end is off by default', () => {
    expect(DEFAULT_SETTINGS).toEqual({ enabled: true, closeAtSessionEnd: false });
  });

  it('does nothing with the guard off', () => {
    const n = note({ expiry: NOW + DAY });
    expect(plan([n], NOW, { ...on, enabled: false })).toEqual([]);
  });

  it('shows a notice once per level, then a close at 5 days', () => {
    const n = note({ expiry: NOW + 4 * DAY });
    const first = plan([n], NOW, on);
    expect(first.map((a) => a.kind)).toEqual(['notice', 'close']);
    expect(first[0]).toMatchObject({ level: 7 });
    // The notice was recorded and the close was tried: nothing repeats within the hour.
    const seen = note({ expiry: NOW + 4 * DAY, guard: { notice: 7, lastTry: NOW } });
    expect(plan([seen], NOW + 60, on)).toEqual([]);
    // After the hour the close is tried again.
    expect(plan([seen], NOW + RETRY_SECS, on).map((a) => a.kind)).toEqual(['close']);
  });

  it('files an exit at 3 days once the close has failed, a close first if it never ran', () => {
    const fresh = note({ expiry: NOW + 2 * DAY });
    expect(plan([fresh], NOW, on).map((a) => a.kind)).toEqual(['notice', 'close']);
    const failed = note({ expiry: NOW + 2 * DAY, guard: { notice: 2, closeFailed: true } });
    expect(plan([failed], NOW, on).map((a) => a.kind)).toEqual(['exit']);
  });

  it('leaves notes that already left alone, and finishes an exit that was never filed', () => {
    const closed = note({ expiry: NOW + DAY, leave: { state: 'closed', at: NOW } });
    const exiting = note({
      expiry: NOW + DAY,
      leave: { state: 'exiting', at: NOW, deadline: NOW + 7 * DAY },
    });
    expect(plan([closed, exiting], NOW, on)).toEqual([]);
    const stuck = note({ expiry: NOW + DAY, leave: { state: 'exiting', at: NOW } });
    expect(plan([stuck], NOW, on).map((a) => a.kind)).toEqual(['exit']);
  });

  it('watches nothing far from expiry', () => {
    expect(plan([note({ expiry: NOW + 20 * DAY })], NOW, on)).toEqual([]);
  });
});

describe('close screen wording', () => {
  it('has the guarantee sentence of the web spec, word for word', () => {
    expect(plain(PUBLIC_SENTENCE)).toBe(
      'The operator cannot send your money to another account or block your exit: with request_exit you leave to your payout account after 7 days, and the operator can deduct at most the bound of the one lease in flight. If you neither close the note nor ask to exit within 30 days, the deposit goes to the treasury. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC.',
    );
  });

  it('says what the sentence does not cover and what closing shows', () => {
    expect(OLD_STATE_LIMIT).toMatch(/old state can be defeated/);
    expect(OLD_STATE_LIMIT).toMatch(/lose your latest state/);
    expect(CLOSE_REVEALS).toMatch(
      /note ID.*total spend.*payout account.*on chain.*account that sends the close/,
    );
  });
});

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

describe('operator close routes', () => {
  it('posts the clearance request and returns the answer text untouched', async () => {
    const seen: { url: string; body: string }[] = [];
    const op = new OperatorClient('http://op.test', async (url, init) => {
      seen.push({ url: String(url), body: String(init?.body) });
      return new Response('{"status":"signed","signature":{"r_x":"0x1"}}');
    });
    const out = await op.clearance('{"withdrawal_nullifier":"0x7"}');
    expect(out).toBe('{"status":"signed","signature":{"r_x":"0x1"}}');
    expect(seen).toEqual([
      { url: 'http://op.test/v2/withdraw/clearance', body: '{"withdrawal_nullifier":"0x7"}' },
    ]);
  });

  it('sends exactly proof and inputs to /v1/close and reads the hash', async () => {
    let body = '';
    const op = new OperatorClient('http://op.test/', async (_url, init) => {
      body = String(init?.body);
      return json(200, { tx_hash: 'HASH' });
    });
    expect(await op.close('PROOF', ['a', 'b'])).toBe('HASH');
    expect(JSON.parse(body)).toEqual({ proof: 'PROOF', inputs: ['a', 'b'] });
  });

  it('keeps the operator’s code on a refusal and says what to do', async () => {
    const op = new OperatorClient('http://op.test', async () =>
      json(409, { error: { code: 'lease_open', message: 'x' } }),
    );
    const error = await op.clearance('{}').catch((e: unknown) => e);
    expect(error).toBeInstanceOf(OperatorError);
    expect((error as OperatorError).code).toBe('lease_open');
    expect(describeOperatorError(error as OperatorError)).toMatch(/lease is still open/);
    for (const code of [
      'nullifier_in_exit',
      'note_not_active',
      'close_rate_limited',
      'emergency',
    ]) {
      expect(describeOperatorError(new OperatorError(503, code, 'm'))).not.toMatch(/answered 503/);
    }
  });
});

describe('vault reads for leaving', () => {
  const view = (rows: unknown[][]) => {
    const calls: unknown[] = [];
    const rpc = new RpcClient(['http://rpc.test'], {
      fetchImpl: async (_url, init) => {
        const request = JSON.parse(String(init?.body)) as { params: { args_base64: string } };
        const args = JSON.parse(atob(request.params.args_base64)) as { from_note_id: number };
        calls.push(args);
        const page = rows[calls.length - 1] ?? [];
        const bytes = Array.from(new TextEncoder().encode(JSON.stringify(page)));
        return json(200, { jsonrpc: '2.0', id: 'x', result: { result: bytes } });
      },
    });
    return { rpc, calls };
  };
  const row = (id: number) => ({
    note_id: id,
    nullifier: '0x1',
    next_nullifier: '0x2',
    final_balance: '1',
    conceded: '0',
    deadline: '1807000000000000000',
  });

  it('pages exit_requests from 0 and finds the note', async () => {
    const full = Array.from({ length: 100 }, (_, i) => row(i));
    const { rpc, calls } = view([full, [row(100), row(101)]]);
    const found = await exitRequest(rpc, 'vault.test', 101);
    expect(found?.note_id).toBe(101);
    expect(calls).toEqual([
      { from_note_id: 0, limit: 100 },
      { from_note_id: 100, limit: 100 },
    ]);
    expect(deadlineSecs(found!)).toBe(1_807_000_000);
  });

  it('answers undefined when the note has no exit', async () => {
    const { rpc } = view([[row(3)]]);
    expect(await exitRequest(rpc, 'vault.test', 4)).toBeUndefined();
  });
});
