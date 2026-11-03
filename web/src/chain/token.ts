// Checks on the USDC token the deposit goes through. These reads name the payout account (and the
// vault) to the RPC provider; the Fund screen says so before it makes them.
import { RpcError, type RpcClient } from './rpc';

/** NEP-145: null when the account is not registered with the token. */
export async function isRegistered(
  rpc: RpcClient,
  token: string,
  account: string,
): Promise<boolean> {
  const balance = await rpc.view<{ total: string } | null>(token, 'storage_balance_of', {
    account_id: account,
  });
  return balance !== null;
}

export async function tokenBalance(
  rpc: RpcClient,
  token: string,
  account: string,
): Promise<bigint> {
  return BigInt(await rpc.view<string>(token, 'ft_balance_of', { account_id: account }));
}

export async function accountExists(rpc: RpcClient, account: string): Promise<boolean> {
  try {
    await rpc.viewAccount(account);
    return true;
  } catch (error) {
    if (error instanceof RpcError && error.code === 'UNKNOWN_ACCOUNT') return false;
    throw error;
  }
}

export interface PayoutAssessment {
  registered: boolean;
  /** The account already exists and holds USDC: an account with a history. */
  fresh: boolean;
  isSender: boolean;
}

/**
 * A payout account is "fresh" when it does not exist yet or exists with no USDC. A different
 * account that is unregistered would leave the payout pending until someone registers it.
 */
export async function assessPayout(
  rpc: RpcClient,
  token: string,
  payout: string,
  sender: string,
): Promise<PayoutAssessment> {
  const isSender = payout === sender;
  const registered = await isRegistered(rpc, token, payout);
  let fresh = true;
  if (registered) fresh = (await tokenBalance(rpc, token, payout)) === 0n;
  return { registered, fresh, isSender };
}

/** A short fingerprint of an account id for the "is this the account you meant" step. */
export async function accountFingerprint(account: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(account));
  const bytes = new Uint8Array(digest).slice(0, 6);
  return [...bytes]
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
    .replace(/(.{4})(?=.)/g, '$1 ');
}
