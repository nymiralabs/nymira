// The wallet list, built from this bundle: the NEAR Connect manifest entries of the wallets the
// profile offers, with each executor served from this origin (`/executors/<file>`) and pinned by
// SHA-256 in `pins/executors.json`. Nothing here is fetched: NEAR Connect gets the manifest as an
// object, and the pinned fetch (pinned-fetch.ts) refuses an executor whose hash differs.
//
// Descriptions, features and permissions are copied from the NEAR Connect repository's manifest
// (hot-dao/near-selector, `repository/manifest.json`) at the retrieval date in executors.json.
// Development-only `allowsOpen` entries (local and staging wallet URLs, a dev app scheme) are left
// out: nothing in a deployed app should let a wallet frame open those.
import type { WalletManifest } from '@hot-labs/near-connect';
import executors from '../../pins/executors.json';
import { PROFILE } from '../profile';

export interface ExecutorPin {
  name: string;
  file: string;
  sha256: string;
  bytes: number;
  source: string;
  manifestVersion: string;
  retrieved: string;
}

export const EXECUTOR_PINS: Record<string, ExecutorPin> = {
  ...(executors.wallets as Record<string, ExecutorPin>),
  // Test-only executors, from the mock profile; empty in a testnet or mainnet bundle.
  ...(PROFILE.extraExecutors as Record<string, ExecutorPin>),
};

const FEATURES: WalletManifest['features'] = {
  signMessage: true,
  signTransaction: false,
  signInWithoutAddKey: true,
  signInAndSignMessage: true,
  signInWithFunctionCallKey: false,
  signAndSendTransaction: true,
  signAndSendTransactions: true,
  signDelegateActions: false,
  testnet: true,
  mainnet: true,
};

// No remote icons: the wallet frame's policy allows `data:` images only.
const ICON =
  'data:image/svg+xml,%3Csvg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 8 8%22%3E%3Crect width=%228%22 height=%228%22 fill=%22%23446%22/%3E%3C/svg%3E';

const PERMISSIONS: Record<string, Record<string, unknown>> = {
  'meteor-wallet': {
    storage: true,
    allowsOpen: [
      'https://chromewebstore.google.com',
      'https://wallet.meteorwallet.app',
      'https://meteorwallet.app',
      'meteorwallet://',
    ],
    external: ['meteorCom', 'meteorComV2'],
  },
  'intear-wallet': {
    storage: true,
    location: true,
    allowsOpen: [
      'https://wallet.intear.tech',
      'intear://',
      'https://logout-bridge-service.intear.tech',
    ],
  },
};

export function manifestEntry(id: string): WalletManifest {
  const pin = EXECUTOR_PINS[id];
  if (!pin) throw new Error(`no executor pin for wallet ${id}`);
  return {
    id,
    name: pin.name,
    icon: ICON,
    description: pin.name,
    website: '',
    version: `${pin.manifestVersion}+${pin.sha256.slice(0, 12)}`,
    executor: `/executors/${pin.file}`,
    type: 'sandbox',
    platform: ['web'],
    features: FEATURES,
    permissions: PERMISSIONS[id] ?? { storage: true },
  };
}
