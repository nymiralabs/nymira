// The note worker: the only place a plain note secret exists. It derives the wrapping key from
// the passphrase (Argon2id), creates secrets with the zkapi wallet core, builds genesis states and
// encrypts and decrypts backup files. Results leave as sealed bytes, public values or a
// non-extractable CryptoKey.
// `@zkapi` is the ceremony-setup build in a deployed profile and the dev-setup build in the mock
// profile (vite.config.ts).
import init, {
  BrowserNoteTree,
  BrowserRequestProver,
  browser_complete_response,
  browser_confirm_deposit,
  browser_generate_deposit,
  browser_prepare_withdrawal,
} from '@zkapi/zkapi_browser.js';
import initCodec, {
  contractAddress,
  destination,
} from '../wasm/near-codec/nymira_near_codec_wasm.js';
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
import {
  completeLease,
  discardPending,
  leaseOpened,
  prepareLease,
  readNote,
  recordExchange,
  type LeaseConfig,
  type LeaseWasm,
} from '../note/lease';
import { prepareWithdrawal, type WithdrawalWasm } from '../note/exit';
import { solvePuzzle } from '../chat/puzzle';
import { sha256Hex } from '../crypto/bytes';
import type { LeaseConfigInput, WorkerRequest, WorkerResponse } from './protocol';

const wasm: NoteWasm = {
  generateDeposit: browser_generate_deposit,
  confirmDeposit: browser_confirm_deposit,
};
const ready = Promise.all([init(), initCodec()]);

// The request prover is loaded once, from the key the page names, after its SHA-256 matches.
let prover: InstanceType<typeof BrowserRequestProver> | null = null;
const leaseWasm: LeaseWasm = {
  prepareRequest(config, state, args) {
    if (!prover) throw new Error('the proving key is not loaded yet');
    return prover.prepare_request(config, state, args);
  },
  treePath: (snapshot, noteId) => BrowserNoteTree.from_state(snapshot).path(noteId),
  completeResponse: browser_complete_response,
};

// The withdrawal proving key (about 4 MB) is read once too, after its SHA-256 matches.
let withdrawalKey: Uint8Array | null = null;
const exitWasm: WithdrawalWasm = {
  prepareWithdrawal(config, state, args) {
    if (!withdrawalKey) throw new Error('the withdrawal proving key is not loaded yet');
    return browser_prepare_withdrawal(config, state, args, withdrawalKey);
  },
};

async function loadKey(url: string, sha256: string, what: string): Promise<Uint8Array> {
  const response = await fetch(url, { credentials: 'omit', cache: 'force-cache' });
  if (!response.ok) throw new Error(`the ${what} could not be loaded (HTTP ${response.status})`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  if ((await sha256Hex(bytes)) !== sha256)
    throw new Error(`the ${what} does not match the SHA-256 this build pins; it is refused`);
  return bytes;
}

const leaseConfig = (c: LeaseConfigInput): LeaseConfig => ({
  chainId: c.chainId,
  operatorPk: c.operatorPk,
  clearancePk: c.clearancePk,
  contractAddress: '0x' + toHex(contractAddress(c.vault)),
});

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
    case 'loadProver': {
      const started = performance.now();
      const response = await fetch(request.url, { credentials: 'omit', cache: 'force-cache' });
      if (!response.ok)
        throw new Error(`the proving key could not be loaded (HTTP ${response.status})`);
      const bytes = new Uint8Array(await response.arrayBuffer());
      if ((await sha256Hex(bytes)) !== request.sha256) {
        throw new Error(
          'the proving key does not match the SHA-256 this build pins; it is refused',
        );
      }
      prover = new BrowserRequestProver(bytes);
      return { bytes: bytes.length, ms: Math.round(performance.now() - started) };
    }
    case 'loadWithdrawalProver': {
      const started = performance.now();
      withdrawalKey = await loadKey(request.url, request.sha256, 'withdrawal proving key');
      return { bytes: withdrawalKey.length, ms: Math.round(performance.now() - started) };
    }
    case 'prepareWithdrawal': {
      const { input } = request;
      const { payout, ...rest } = input;
      return prepareWithdrawal(exitWasm, request.key, request.aad, request.sealed, {
        ...rest,
        config: leaseConfig(input.config),
        ...(payout ? { destination: '0x' + toHex(destination(payout)) } : {}),
      });
    }
    case 'prepareLease': {
      const { input } = request;
      return prepareLease(leaseWasm, request.key, request.aad, request.sealed, {
        ...input,
        config: leaseConfig(input.config),
      });
    }
    case 'solvePuzzle':
      return solvePuzzle(request.nonce, request.difficulty);
    case 'leaseOpened':
      return leaseOpened(request.key, request.aad, request.sealed, request.lease);
    case 'recordExchange':
      return recordExchange(request.key, request.aad, request.sealed, request.use);
    case 'completeLease':
      return completeLease(
        leaseWasm,
        request.key,
        request.aad,
        request.sealed,
        leaseConfig(request.config),
        request.settlement,
      );
    case 'discardPending':
      return discardPending(request.key, request.aad, request.sealed);
    case 'readNote':
      return readNote(request.key, request.aad, request.sealed);
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
