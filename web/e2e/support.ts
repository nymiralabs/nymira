// Helpers for the e2e tests: the scripted fake chain, a test wallet that records what a real
// NEAR transaction would do, and small page drivers.
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, type BrowserContext, type Page } from '@playwright/test';
// The test wallet of issue #86 (web-testkit); resolved by the tsconfig path `@testkit`.
import { NearTestWallet, installTestWallet } from '@testkit';
import { decryptBackup } from '../src/backup/format';
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

export type WalletMode = 'tree' | 'bare' | 'refused' | 'crash';

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
    if (this.mode === 'crash') throw new Error('the wallet window was closed');
    if (this.mode === 'bare') return { transaction: { hash, signer_id: SENDER } };
    return this.rpc('tx', { tx_hash: hash, sender_account_id: SENDER, wait_until: 'FINAL' });
  }
}

export async function newFundingWallet(
  context: BrowserContext,
  script: VaultScript = {},
): Promise<FundingWallet> {
  const wallet = new FundingWallet();
  await scriptChain(wallet, script);
  await installTestWallet(context, wallet);
  return wallet;
}

// ---- page drivers ----

export async function openFund(page: Page) {
  await page.goto('/');
  await page.getByTestId('open-app').click();
  await expect(page.getByTestId('fund')).toBeVisible();
}

/** Opens the Notes tab; unlocks first when `unlock` is set (after a reload). */
export async function openNotes(page: Page, unlock = false) {
  await page.getByTestId('nav-notes').click();
  if (unlock) {
    await page.getByTestId('unlock-passphrase').fill(WRAP_PASSPHRASE);
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

export async function setUpPassphrase(page: Page, passphrase = WRAP_PASSPHRASE) {
  await page.getByTestId('setup-passphrase').fill(passphrase);
  await page.getByTestId('setup-confirm').fill(passphrase);
  await page.getByTestId('setup-submit').click();
  await expect(page.getByTestId('prepare')).toBeVisible();
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
