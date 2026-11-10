import 'fake-indexeddb/auto';
import { expect, it } from 'vitest';
import { confirmDeposit } from '../src/funding/flow';
import { getNote, putNote } from '../src/store/db';
import type { NoteRecord } from '../src/store/types';
import type { RpcClient } from '../src/chain/rpc';

it('preserves the note when a deposit event has a different payout account', async () => {
  const commitment = '0x' + 'ab'.repeat(32);
  const record = {
    id: 'mismatch',
    vault: 'vault.test',
    sender: 'alice.test',
    payout: 'alice.test',
    commitment,
    denomination: '1000000',
    kind: 'pending',
    phase: 'sent',
    txHash: 'hash',
  } as NoteRecord;
  await putNote(record);
  const outcome = {
    transaction: { hash: 'hash', signer_id: 'alice.test' },
    receipts_outcome: [
      {
        outcome: {
          executor_id: 'vault.test',
          status: { SuccessValue: '' },
          logs: [
            'EVENT_JSON:' +
              JSON.stringify({
                standard: 'nymira',
                version: '1.0.0',
                event: 'deposit',
                data: {
                  note_id: 1,
                  commitment,
                  denomination: '1000000',
                  expiry: 1795000000,
                  withdraw_to: 'mallory.test',
                },
              }),
          ],
        },
      },
    ],
  };
  const result = await confirmDeposit({} as RpcClient, record, outcome);
  expect(result.status).toBe('pending');
  expect((await getNote(record.id))?.phase).toBe('sent');

  const rpc = { txStatus: async () => outcome } as unknown as RpcClient;
  const fromRpc = await confirmDeposit(rpc, record, { transaction: { hash: 'hash' } });
  expect(fromRpc.status).toBe('pending');
  expect((await getNote(record.id))?.phase).toBe('sent');
});
