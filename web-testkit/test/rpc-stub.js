// A tiny JSON-RPC stub with just what the test wallet calls, and a check that a
// transaction it received carries a valid signature.

import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { SignedTransaction, baseEncode } from 'near-api-js';

export const BLOCK_HASH = baseEncode(new Uint8Array(32).fill(7));

export async function startRpcStub({ nonce = 41 } = {}) {
  const received = [];
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      const { id, method, params } = JSON.parse(body);
      let result;
      if (method === 'query' && params.request_type === 'view_access_key') {
        received.push({ method, params });
        result = { nonce, permission: 'FullAccess', block_height: 1, block_hash: BLOCK_HASH };
      } else if (method === 'block') {
        result = { header: { hash: BLOCK_HASH, height: 1 } };
      } else if (method === 'send_tx') {
        received.push({ method, params });
        result = { final_execution_status: 'FINAL', status: { SuccessValue: '' } };
      } else {
        res.end(JSON.stringify({ jsonrpc: '2.0', id, error: { code: -32601, message: `no ${method}` } }));
        return;
      }
      res.end(JSON.stringify({ jsonrpc: '2.0', id, result }));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    /** The `send_tx` calls received, in order. */
    sent: () => received.filter((r) => r.method === 'send_tx'),
    all: received,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

/** Decodes a `send_tx` payload and checks its ed25519 signature against the signer's key. */
export function decodeAndVerify(signedTxBase64) {
  const signed = SignedTransaction.decode(Buffer.from(signedTxBase64, 'base64'));
  const digest = createHash('sha256').update(signed.transaction.encode()).digest();
  const valid = signed.transaction.publicKey.verify(
    digest,
    Uint8Array.from(signed.signature.ed25519Signature.data),
  );
  return { transaction: signed.transaction, valid };
}
