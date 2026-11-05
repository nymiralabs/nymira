// Helpers for the close and exit tests. What is real here: the app, the worker's proofs (the
// development setup), the operator mock's verification of the ownership proof and the clearance it
// signs with a real clearance key, the test wallet's signed transactions sent to the fake RPC, and
// the check of every close and exit call against the development withdrawal verifying key from the
// 16 statement words, the way the vault checks them.
// What is not: the vault. The fake RPC runs no contract, so after each call this wallet scripts the
// views the vault would show (`note_records`, `exit_requests`, `pending_payout`), and the chain's
// clock is the fake chain's, moved by the test (the sandbox cannot move seven days).
import { expect, type Page } from '@playwright/test';
import { NearTestWallet } from '@testkit';
import { decodeCall, type DecodedCall, type MockStack } from './mock-operator';
import { FundingWallet, RPC, VAULT, control } from './support';

export const DAY = 86_400;

export const rpcCall = async (method: string, params: unknown) => {
  const r = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  return ((await r.json()) as { result: Record<string, unknown> }).result;
};

/** The fake chain's final block time, in seconds. */
export async function chainNow(): Promise<number> {
  const block = (await rpcCall('block', { finality: 'final' })) as {
    header: { timestamp_nanosec: string };
  };
  return Number(BigInt(block.header.timestamp_nanosec) / 1_000_000_000n);
}

/** Moves the fake chain forward by `seconds` in one block. */
export const advanceChain = (seconds: number) =>
  control({ cmd: 'advance_blocks', n: 1, seconds_per_block: seconds });

const view = (method: string, result: unknown, account = VAULT) =>
  control({ cmd: 'set_view', account, method, args: 'any', result: { json: result } });

export const showNoteRecord = (
  status: 'Active' | 'PendingExit' | 'Closed',
  noteId = 0,
  expiry = 0,
  deposit = '1000000',
) =>
  view('note_records', [
    { note_id: noteId, deposit_amount: deposit, expiry, status, leaf_retired: false },
  ]);

export const showExits = (rows: unknown[]) => view('exit_requests', rows);

export const showPending = (account: string, amount: string) =>
  control({
    cmd: 'set_view',
    account: VAULT,
    method: 'pending_payout',
    args: { json: { account_id: account } },
    result: { json: amount },
  });

export interface VaultCall {
  method: string;
  args: Record<string, unknown>;
  deposit: string;
  gas: string;
}

/**
 * The funding wallet that also sends the vault's close, exit, finalize and claim calls, records
 * them and scripts what the vault would show afterwards.
 */
export class LeaveWallet extends FundingWallet {
  vaultCalls: VaultCall[] = [];
  /** While set, a vault call waits for this promise: the wallet's window is "open". */
  hold: Promise<void> | null = null;
  /** The payout a finalize leaves pending (the payout transfer "failed"). */
  pendingAfterFinalize = '0';
  expiry = 0;
  /**
   * What a `mutual_close` does: `silent` lands and the wallet never answers; `dead` sends nothing
   * and never answers; `cancel` is a user who says no.
   */
  closeMode: 'normal' | 'silent' | 'dead' | 'cancel' = 'normal';

  override async signAndSendTransaction(params: {
    receiverId: string;
    actions: { type: string; params: Record<string, unknown> }[];
    signerId?: string;
  }) {
    if (params.receiverId !== VAULT) return super.signAndSendTransaction(params as never);
    const action = params.actions[0]!.params as {
      methodName: string;
      args: Record<string, unknown>;
      deposit: string;
      gas: string;
    };
    const never = () => new Promise<never>(() => undefined);
    const closing = action.methodName === 'mutual_close';
    if (closing && this.closeMode === 'cancel') throw new Error('User rejected the transaction');
    if (closing && this.closeMode === 'dead') return never();
    if (this.hold) await this.hold;
    this.vaultCalls.push({
      method: action.methodName,
      args: action.args,
      deposit: String(action.deposit),
      gas: String(action.gas),
    });
    const send = Object.getPrototypeOf(FundingWallet.prototype).signAndSendTransaction as (
      this: NearTestWallet,
      p: unknown,
    ) => Promise<unknown>;
    const outcome = await send.call(this, params);
    await this.vaultEffect(action.methodName, action.args);
    if (closing && this.closeMode === 'silent') return never();
    return outcome;
  }

  /** What the vault would now show. */
  private async vaultEffect(method: string, args: Record<string, unknown>) {
    const payout = this.lastDeposit.withdraw_to as string;
    switch (method) {
      case 'mutual_close': {
        await showNoteRecord('Closed', 0, this.expiry);
        break;
      }
      case 'request_exit': {
        const call = decodeCall(args as { proof: string; inputs: string[] });
        await showNoteRecord('PendingExit', call.noteId, this.expiry);
        await showExits([
          {
            note_id: call.noteId,
            nullifier: call.nullifier,
            next_nullifier: call.nextNullifier,
            final_balance: call.finalBalance.toString(),
            conceded: call.conceded.toString(),
            deadline: String(BigInt((await chainNow()) + 7 * DAY) * 1_000_000_000n),
          },
        ]);
        break;
      }
      case 'finalize_exit': {
        await showExits([]);
        await showNoteRecord('Closed', 0, this.expiry);
        if (this.pendingAfterFinalize !== '0') await showPending(payout, this.pendingAfterFinalize);
        break;
      }
      case 'claim_payout': {
        await showPending(args.account_id as string, '0');
        break;
      }
    }
  }

  /** The call `method` sent last, decoded the way the vault reads it. */
  decoded(method: string): DecodedCall {
    const call = this.vaultCalls.findLast((c) => c.method === method);
    if (!call) throw new Error(`no ${method} call was sent`);
    return decodeCall(call.args as { proof: string; inputs: string[] });
  }
}

/** The funded note's vault records and views before anything happens to it. */
export async function showActive(wallet: LeaveWallet, expiry: number) {
  wallet.expiry = expiry;
  await showNoteRecord('Active', 0, expiry);
  await showExits([]);
  await showPending(wallet.accountId, '0');
}

export async function openClose(page: Page) {
  await page.getByTestId('nav-close').click();
  await expect(page.getByTestId('close-screen')).toBeVisible();
  await expect(page.getByTestId('withdrawal-prover-status')).toContainText(
    'Withdrawal proving key loaded and checked',
  );
}

/** Operator mock plumbing for the operator-sent close: the chain shows the note closed afterwards. */
export function operatorClosesOnChain(stack: MockStack, expiry: number) {
  stack.onClose = async () => {
    await showNoteRecord('Closed', 0, expiry);
  };
}
