import type { WalletInfo } from '../wallet/protocol';

/**
 * The Agents screen signs messages, so it needs a wallet that can. It is hidden when the connected
 * wallet cannot, and before any wallet is connected when none of the listed wallets can. An empty
 * list means the wallet frame has not answered yet; the screen stays visible until it has.
 */
export function agentsAvailable(wallets: WalletInfo[], connectedWalletId: string | null): boolean {
  if (connectedWalletId) {
    const connected = wallets.find((w) => w.id === connectedWalletId);
    return connected?.signMessage ?? false;
  }
  return wallets.length === 0 || wallets.some((w) => w.signMessage);
}
