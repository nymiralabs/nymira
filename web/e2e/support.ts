// Helpers for the e2e tests: the scripted fake chain, a test wallet that records what a real
// NEAR transaction would do, and small page drivers.
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, type BrowserContext, type Page } from '@playwright/test';
// The test wallet of issue #86 (web-testkit); resolved by the tsconfig path `@testkit`.
import { NearTestWallet, installTestWallet } from '@testkit';
import { decryptBackup } from '../src/backup/format';
import { pad32 } from '../src/note/core';
import {
  browser_note_leaf_from_fields,
  initSync,
} from '../src/wasm/zkapi-browser/zkapi_browser.js';
const fixture = JSON.parse(
  readFileSync(new URL('../test-fixtures/mock-vault.json', import.meta.url), 'utf8'),
);

export const RPC = 'http://127.0.0.1:18545';
export const ONECLICK = 'http://127.0.0.1:18547';
export const BACKUP_PASSPHRASE = 'backup passphrase 1234';
export const WRAP_PASSPHRASE = 'wrapping passphrase 1234';
export const SENDER = fixture.senderAccount;
export const OTHER = fixture.otherPayoutAccount;
export const VAULT = fixture.vaultAccount;
export const TOKEN = fixture.tokenAccount;

export async function control(cmd: Record<string, unknown>) {
  const response = await fetch(`${RPC}/control`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(cmd),
  });
  if (!response.ok) throw new Error(`control ${JSON.stringify(cmd)}: ${await response.text()}`);
}

export const rpcRequests = async (): Promise<{ method: string; params: unknown }[]> =>
  (await fetch(`${RPC}/control/requests`)).json();

export interface VaultScript {
  codeHash?: string;
  vkHashes?: { request: string; withdrawal: string };
  configOverrides?: Record<string, unknown>;
  accessKeys?: { public_key: string; nonce: number; permission: unknown }[];
  tvlCap?: string;
  currentTvl?: string;
  /** Accounts registered with the token. */
  registered?: string[];
  balances?: Record<string, string>;
}

const json = (value: unknown) => ({ json: value });

/** Scripts the fake chain: the vault's views, the token's views, the wallet's access key. */
export async function scriptChain(wallet: NearTestWallet, script: VaultScript = {}) {
  await control({ cmd: 'reset' });
  const pins = fixture.pins;
  const view = (account: string, method: string, args: unknown, result: unknown) =>
    control({
      cmd: 'set_view',
      account,
      method,
      args: args === 'any' ? 'any' : json(args),
      result: json(result),
    });
  await control({
    cmd: 'set_account',
    account: VAULT,
    spec: {
      amount: '1000000000000000000000000000',
      locked: '0',
      code_hash: script.codeHash ?? pins.codeHash,
      storage_usage: 500000,
    },
  });
  await control({ cmd: 'set_access_keys', account: VAULT, keys: script.accessKeys ?? [] });
  await view(VAULT, 'leaf_count', 'any', 0);
  await view(VAULT, 'note_records', 'any', []);
  await view(VAULT, 'get_leaves', 'any', []);
  await view(VAULT, 'vk_hashes', 'any', script.vkHashes ?? pins.vkHashes);
  await view(VAULT, 'config', 'any', {
    ...pins.config,
    ...script.configOverrides,
    tvl_cap: script.tvlCap ?? '100000000',
    current_tvl: script.currentTvl ?? '0',
  });
  const registered = new Set(script.registered ?? [SENDER, VAULT]);
  for (const account of [SENDER, OTHER, VAULT, 'carol.test']) {
    await view(
      TOKEN,
      'storage_balance_of',
      { account_id: account },
      registered.has(account) ? { total: '1250000000000000000000', available: '0' } : null,
    );
    await view(TOKEN, 'ft_balance_of', { account_id: account }, script.balances?.[account] ?? '0');
  }
  await control({ cmd: 'set_account', account: wallet.accountId, spec: {} });
  await control({
    cmd: 'set_access_keys',
    account: wallet.accountId,
    keys: [{ public_key: wallet.publicKey, nonce: 0, permission: 'FullAccess' }],
  });
}

const DAY = 86_400;
/** The vault's rule: 30 days ahead, rounded up to the end of a UTC day. */
export function expectedExpiry(nowSeconds = Math.floor(Date.now() / 1000)): number {
  return (Math.floor((nowSeconds + 30 * DAY) / DAY) + 1) * DAY;
}

let wasmReady = false;
/** The vault's leaf for a deposit, from the wallet core the app runs (Node build). */
export function leafFor(noteId: number, commitment: string, amount: string, expiry: number) {
  if (!wasmReady) {
    initSync({
      module: readFileSync(
        new URL('../src/wasm/zkapi-browser/zkapi_browser_bg.wasm', import.meta.url),
      ),
    });
    wasmReady = true;
  }
  return pad32(
    JSON.parse(browser_note_leaf_from_fields(noteId, commitment, amount, String(expiry))),
  );
}

export type WalletMode =
  | 'tree'
  | 'bare'
  | 'refused'
  | 'crash'
  /** The transaction lands, the vault shows the note, and the wallet never answers. */
  | 'silent'
  /** Like `silent`, then the user closes the wallet window: the wallet rejects. */
  | 'silent-closed'
  /** The user cancels in the wallet: nothing is sent. */
  | 'cancel'
  /** Nothing is sent and the wallet never answers. */
  | 'dead';

/**
 * The test wallet plus the chain's side of a deposit: after it sends the real transaction to the
 * fake RPC it scripts the receipts a vault would produce (a `deposit` event), and returns what
 * `mode` says a wallet might return:
 *   tree     the full receipt tree
 *   bare     only the transaction hash
 *   refused  a complete outcome without a `deposit` event (the token refunded the transfer)
 *   crash    nothing: the wallet window "closes" after the transaction went out
 */
export class FundingWallet extends NearTestWallet {
  mode: WalletMode = 'tree';
  nextNoteId = 0;
  /** Changes the event the vault "emits", to play a tampered page or wallet. */
  eventOverride: Record<string, unknown> = {};
  /** Transactions the app asked for, as the wallet saw the call. */
  calls: { receiverId: string; args: Record<string, unknown> }[] = [];
  lastHash = '';
  /** The `deposit` event data the vault "emitted" last. */
  lastDeposit: Record<string, unknown> = {};

  constructor() {
    super({ accountId: SENDER, rpcUrl: RPC, network: 'testnet' });
  }

  override async signAndSendTransaction(params: {
    receiverId: string;
    actions: { type: string; params: { args: Record<string, string> } }[];
    signerId?: string;
  }) {
    const args = params.actions[0]?.params.args as Record<string, string>;
    this.calls.push({ receiverId: params.receiverId, args });
    if (this.mode === 'cancel') throw new Error('User rejected the transaction');
    if (this.mode === 'dead') return new Promise<never>(() => undefined);
    const outcome = await super.signAndSendTransaction(params as never);
    const hash: string = outcome.transaction.hash;
    this.lastHash = hash;

    const msg = JSON.parse(args.msg ?? '{}') as { commitment: string; withdraw_to?: string };
    const denomination = (
      BigInt(args.amount ?? '0') - BigInt(fixture.pins.config.note_fee)
    ).toString();
    const event = {
      standard: 'nymira',
      version: '1.0.0',
      event: 'deposit',
      data: {
        note_id: this.nextNoteId++,
        commitment: msg.commitment,
        denomination,
        expiry: expectedExpiry(),
        withdraw_to: msg.withdraw_to ?? SENDER,
        ...this.eventOverride,
      },
    };
    this.lastDeposit = event.data;
    const logs = this.mode === 'refused' ? [] : [`EVENT_JSON:${JSON.stringify(event)}`];
    await control({
      cmd: 'set_tx_status',
      status: {
        tx_hash: hash,
        signer_id: SENDER,
        receiver_id: params.receiverId,
        nonce: 1,
        receipts: [
          { executor_id: params.receiverId, logs: [] },
          { executor_id: VAULT, logs },
          { executor_id: params.receiverId, logs: [] },
        ],
      },
    });
    if (this.mode === 'silent' || this.mode === 'silent-closed') {
      const data = event.data as { note_id: number; expiry: number };
      const view = (method: string, result: unknown) =>
        control({ cmd: 'set_view', account: VAULT, method, args: 'any', result: { json: result } });
      await view('note_records', [
        {
          note_id: data.note_id,
          deposit_amount: denomination,
          expiry: data.expiry,
          status: 'Active',
          leaf_retired: false,
        },
      ]);
      await view('get_leaves', [leafFor(data.note_id, msg.commitment, denomination, data.expiry)]);
      if (this.mode === 'silent') return new Promise<never>(() => undefined);
      // The window stays open for a while, then the user closes it.
      await new Promise((r) => setTimeout(r, 3000));
      throw new Error('User rejected the transaction');
    }
    if (this.mode === 'crash') throw new Error('the wallet window was closed');
    if (this.mode === 'bare') return { transaction: { hash, signer_id: SENDER } };
    return this.rpc('tx', { tx_hash: hash, sender_account_id: SENDER, wait_until: 'FINAL' });
  }
}

export const GATED_WALLET_ID = 'nymira-gated-test-wallet';

export async function newFundingWallet(
  context: BrowserContext,
  script: VaultScript = {},
  options: { gated?: boolean } = {},
): Promise<FundingWallet> {
  const wallet = new FundingWallet();
  // A wallet the mock profile lists under `clickBeforeSend`: the frame asks for a click first.
  if (options.gated) {
    wallet.manifest = { ...wallet.manifest, id: GATED_WALLET_ID, name: 'Gated test wallet' };
  }
  await scriptChain(wallet, script);
  await installTestWallet(context, wallet);
  return wallet;
}

// ---- page drivers ----

/** After a reload the tab reopens on the screen it was on; otherwise the landing page shows. */
export async function enterApp(page: Page) {
  const open = page.getByTestId('open-app');
  await expect(open.or(page.getByTestId('nav-fund'))).toBeVisible();
  if (await open.isVisible()) await open.click();
}

export async function openFund(page: Page, source: 'near' | 'other' = 'near') {
  await page.goto('/');
  await page.getByTestId('open-app').click();
  await expect(page.getByTestId('fund')).toBeVisible();
  // The page starts on the NEAR route; the other chain route is a tile away.
  if (source === 'other') await page.getByTestId('fund-source-other').check({ force: true });
}

/** Opens the Notes tab; unlocks first when `unlock` is set (after a reload). */
export async function openNotes(page: Page, unlock = false) {
  await page.getByTestId('nav-notes').click();
  // A dev build with the mock profile keeps the tab unlocked across a reload: then there is no prompt.
  const field = page.getByTestId('unlock-passphrase');
  if (unlock && (await field.isVisible({ timeout: 3000 }).catch(() => false))) {
    await field.fill(WRAP_PASSPHRASE);
    await page.getByTestId('unlock-submit').click();
  }
  await expect(page.getByTestId('notes')).toBeVisible();
}

export const workdir = () => mkdtempSync(join(tmpdir(), 'nymira-e2e-'));

/** Console lines, page errors and every request (URL and body) the page made. */
export function watch(page: Page) {
  const seen = { console: [] as string[], requests: [] as string[], violations: [] as string[] };
  page.on('console', (m) => {
    seen.console.push(m.text());
    if (/Content Security Policy|Refused to/i.test(m.text())) seen.violations.push(m.text());
  });
  page.on('pageerror', (e) => seen.console.push(e.message));
  page.on('request', (r) => seen.requests.push(`${r.method()} ${r.url()} ${r.postData() ?? ''}`));
  return seen;
}

export async function connectWallet(page: Page) {
  await page.getByTestId('connect-nymira-test-wallet').click();
  await expect(page.getByTestId('connected')).toContainText(SENDER);
}

export async function setUpPassphrase(page: Page, passphrase = WRAP_PASSPHRASE, ready = 'prepare') {
  await page.getByTestId('setup-passphrase').fill(passphrase);
  await page.getByTestId('setup-confirm').fill(passphrase);
  await page.getByTestId('setup-submit').click();
  await expect(page.getByTestId(ready)).toBeVisible();
}

/** Connects, sets the passphrase and creates the note secret; stops before the backup. */
export async function prepare(page: Page) {
  await connectWallet(page);
  await setUpPassphrase(page);
  await expect(page.getByTestId('prepare')).toBeEnabled();
  await page.getByTestId('prepare').click();
  await expect(page.getByTestId('backup-panel')).toBeVisible();
}

/** Downloads the backup and chooses it again; returns the file's path. */
export async function backUpAndTest(page: Page, saveTo: string): Promise<string> {
  await page.getByTestId('backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('backup-download').click(),
  ]);
  await download.saveAs(saveTo);
  await page.getByTestId('backup-file').setInputFiles(saveTo);
  await expect(page.getByTestId('backup-tested')).toBeVisible();
  return saveTo;
}

export const readBackup = async (path: string, passphrase = BACKUP_PASSPHRASE) =>
  decryptBackup(new Uint8Array(readFileSync(path)), passphrase);

/** Everything the page could have written the secret to, as one list of strings. */
export async function pageSurfaces(page: Page): Promise<string[]> {
  const inPage = await page.evaluate(async () => {
    const out: string[] = [];
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open('nymira');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    for (const name of Array.from(db.objectStoreNames)) {
      const rows = await new Promise<unknown[]>((resolve, reject) => {
        const request = db.transaction(name).objectStore(name).getAll();
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      out.push(`idb:${name}:${JSON.stringify(rows)}`);
    }
    db.close();
    out.push(`local:${JSON.stringify({ ...localStorage })}`);
    out.push(`session:${JSON.stringify({ ...sessionStorage })}`);
    out.push(`dom:${document.documentElement.outerHTML}`);
    return out;
  });
  return inPage;
}

/** Forms of one secret a leak could take. */
export function secretForms(secret: string): string[] {
  const digits = secret.replace(/^0x/i, '').toLowerCase();
  const trimmed = digits.replace(/^0+/, '');
  return [...new Set([digits, trimmed, digits.toUpperCase(), trimmed.toUpperCase()])].filter(
    (form) => form.length >= 40,
  );
}

export function expectNoSecret(surfaces: string[], secret: string) {
  for (const form of secretForms(secret)) {
    for (const surface of surfaces) {
      expect(surface.includes(form), `secret found in ${surface.slice(0, 60)}`).toBe(false);
    }
  }
}
