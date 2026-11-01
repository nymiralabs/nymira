// Self-test of the test wallet from plain Node, against an in-test JSON-RPC stub.

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { after, before, test } from 'node:test';
import { PublicKey, baseEncode } from 'near-api-js';
import { NearTestWallet } from '../src/wallet.js';
import { BLOCK_HASH, decodeAndVerify, startRpcStub } from './rpc-stub.js';

let rpc;
let wallet;
before(async () => {
  rpc = await startRpcStub({ nonce: 41 });
  wallet = new NearTestWallet({ accountId: 'alice.testnet', rpcUrl: rpc.url });
});
after(() => rpc.close());

test('signed out until signIn', async () => {
  assert.deepEqual(await wallet.getAccounts(), []);
  await assert.rejects(
    wallet.signAndSendTransaction({ receiverId: 'bob.testnet', actions: [] }),
    /not signed in/,
  );
  const [account] = await wallet.signIn();
  assert.equal(account.accountId, 'alice.testnet');
  assert.equal(account.publicKey, wallet.publicKey);
});

test('signs and sends a FunctionCall with a valid signature', async () => {
  const outcome = await wallet.signAndSendTransaction({
    receiverId: 'usdc.testnet',
    actions: [
      {
        type: 'FunctionCall',
        params: { methodName: 'ft_transfer_call', args: { receiver_id: 'vault.nymira.testnet', amount: '1050000', msg: '{}' }, gas: '100000000000000', deposit: '1' },
      },
    ],
  });
  assert.equal(outcome.status.SuccessValue, '');
  const [call] = rpc.sent().slice(-1);
  assert.equal(call.params.wait_until, 'FINAL');
  const { transaction, valid } = decodeAndVerify(call.params.signed_tx_base64);
  assert.ok(valid, 'signature verifies');
  assert.equal(transaction.signerId, 'alice.testnet');
  assert.equal(transaction.receiverId, 'usdc.testnet');
  assert.equal(transaction.nonce, 42n);
  assert.equal(baseEncode(Uint8Array.from(transaction.blockHash)), BLOCK_HASH);
  assert.equal(transaction.publicKey.toString(), wallet.publicKey);
  const [action] = transaction.actions;
  assert.equal(action.functionCall.methodName, 'ft_transfer_call');
  assert.deepEqual(JSON.parse(Buffer.from(action.functionCall.args).toString()), {
    receiver_id: 'vault.nymira.testnet',
    amount: '1050000',
    msg: '{}',
  });
  assert.equal(action.functionCall.gas, 100000000000000n);
  assert.equal(action.functionCall.deposit, 1n);
  assert.equal(wallet.sent.at(-1).receiverId, 'usdc.testnet');
});

test('signs a Transfer; nonces keep rising when the chain nonce does not', async () => {
  await wallet.signAndSendTransaction({
    receiverId: 'bob.testnet',
    actions: [{ type: 'Transfer', params: { deposit: '1000000000000000000000000' } }],
  });
  const { transaction, valid } = decodeAndVerify(rpc.sent().at(-1).params.signed_tx_base64);
  assert.ok(valid);
  assert.equal(transaction.nonce, 43n);
  assert.equal(transaction.actions[0].transfer.deposit, 10n ** 24n);
});

test('signAndSendTransactions sends in order', async () => {
  const before = rpc.sent().length;
  const outcomes = await wallet.signAndSendTransactions({
    transactions: [
      { receiverId: 'a.testnet', actions: [{ type: 'Transfer', params: { deposit: '1' } }] },
      { receiverId: 'b.testnet', actions: [{ type: 'Transfer', params: { deposit: '2' } }] },
    ],
  });
  assert.equal(outcomes.length, 2);
  const [a, b] = rpc.sent().slice(before).map((c) => decodeAndVerify(c.params.signed_tx_base64).transaction);
  assert.deepEqual([a.receiverId, b.receiverId], ['a.testnet', 'b.testnet']);
  assert.equal(b.nonce, a.nonce + 1n);
});

test('refuses another signer and unsupported actions', async () => {
  await assert.rejects(
    wallet.signAndSendTransaction({ signerId: 'eve.testnet', receiverId: 'x.testnet', actions: [] }),
    /signs for alice.testnet/,
  );
  await assert.rejects(
    wallet.signAndSendTransaction({ receiverId: 'x.testnet', actions: [{ type: 'DeleteAccount', params: {} }] }),
    /unsupported action/,
  );
});

test('NEP-413 message signature verifies', async () => {
  const nonce = new Uint8Array(32).fill(3);
  const signed = await wallet.signMessage({ message: 'hello', recipient: 'nymira.pages.dev', nonce });
  assert.equal(signed.accountId, 'alice.testnet');
  assert.equal(signed.publicKey, wallet.publicKey);
  // Signed bytes: sha256(u32le(2^31 + 413) || borsh({ message, nonce, recipient, callbackUrl: None })).
  const u32 = (n) => Buffer.from(new Uint32Array([n]).buffer);
  const str = (s) => Buffer.concat([u32(Buffer.byteLength(s)), Buffer.from(s)]);
  const payload = Buffer.concat([u32(2 ** 31 + 413), str('hello'), nonce, str('nymira.pages.dev'), Buffer.from([0])]);
  const digest = createHash('sha256').update(payload).digest();
  const signature = Buffer.from(signed.signature, 'base64');
  assert.ok(PublicKey.fromString(wallet.publicKey).verify(digest, signature));
});
