// The app's singletons and the hooks that read them.
import { useEffect, useState, useSyncExternalStore } from 'react';
import { PROFILE } from '../profile';
import { RpcClient } from '../chain/rpc';
import { checkVault, type VaultCheck } from '../chain/vault';
import { walletClient } from '../wallet/client';

export const rpc = new RpcClient(PROFILE.rpcUrls);
/**
 * For receipts that regular nodes no longer hold: the last endpoint of the profile, asked on its
 * own. It is one of the endpoints the CSP already allows.
 */
export const archivalRpc = new RpcClient(PROFILE.rpcUrls.slice(-1));

export function useWallet() {
  useSyncExternalStore(
    (listener) => walletClient.subscribe(listener),
    () => walletClient.version,
  );
  return walletClient;
}

/** The origin this bundle is made for. A copy served from another origin is told to stop. */
export const originOk = location.origin === PROFILE.appOrigin;

export function useVaultCheck(): { check: VaultCheck | null; recheck: () => void } {
  const [check, setCheck] = useState<VaultCheck | null>(null);
  const [round, setRound] = useState(0);
  useEffect(() => {
    // A copy served from another origin makes no request at all.
    if (!originOk) return;
    let live = true;
    checkVault(rpc, PROFILE.vaultAccount, PROFILE.pins).then((result) => {
      if (live) setCheck(result);
    });
    return () => {
      live = false;
    };
  }, [round]);
  return { check, recheck: () => setRound((n) => n + 1) };
}
