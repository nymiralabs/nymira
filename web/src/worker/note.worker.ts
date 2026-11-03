// The note worker: the only place a plain note secret exists. It derives the wrapping key from
// the passphrase (Argon2id), creates secrets with the zkapi wallet core, builds genesis states and
// encrypts and decrypts backup files. Results leave as sealed bytes, public values or a
// non-extractable CryptoKey.
import init, {
  browser_confirm_deposit,
  browser_generate_deposit,
} from '../wasm/zkapi-browser/zkapi_browser.js';
import initCodec, { contractAddress } from '../wasm/near-codec/nymira_near_codec_wasm.js';
import { importAesKey } from '../crypto/aead';
import { b64decode, toHex } from '../crypto/bytes';
import { deriveKeyBytes, type Argon2Params } from '../crypto/kdf';
import {
  confirmNote,
  generateNote,
  makeBackup,
  restoreBackup,
  testBackup,
  type NoteWasm,
} from '../note/core';
import type { WorkerRequest, WorkerResponse } from './protocol';

const wasm: NoteWasm = {
  generateDeposit: browser_generate_deposit,
  confirmDeposit: browser_confirm_deposit,
};
const ready = Promise.all([init(), initCodec()]);

async function handle(request: WorkerRequest): Promise<unknown> {
  await ready;
  switch (request.op) {
    case 'deriveKey': {
      const params: Argon2Params = request.params;
      const bytes = await deriveKeyBytes(request.passphrase, b64decode(request.salt), params);
      const key = await importAesKey(bytes);
      bytes.fill(0);
      return key;
    }
    case 'generate':
      return generateNote(wasm, request.key, request.aad);
    case 'confirm': {
      const { vault, config, ...rest } = request.input;
      const mapped = { ...config, contractAddress: '0x' + toHex(contractAddress(vault)) };
      return confirmNote(wasm, request.key, request.aad, request.pending, {
        ...rest,
        config: mapped,
      });
    }
    case 'makeBackup':
      return makeBackup(
        request.key,
        request.records,
        request.passphrase,
        request.now,
        request.scope,
      );
    case 'testBackup':
      return testBackup(request.key, request.bytes, request.passphrase, request.record);
    case 'restoreBackup':
      return restoreBackup(request.key, request.bytes, request.passphrase, request.scope);
  }
}

self.onmessage = (event: MessageEvent<WorkerRequest & { id: number }>) => {
  const { id, ...request } = event.data;
  handle(request as WorkerRequest).then(
    (result) => {
      const response: WorkerResponse = { id, ok: true, result };
      (self as unknown as Worker).postMessage(response);
    },
    (error: unknown) => {
      const response: WorkerResponse = {
        id,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      };
      (self as unknown as Worker).postMessage(response);
    },
  );
};
