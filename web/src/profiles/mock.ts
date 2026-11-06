// The profile the tests run against: the fake NEAR RPC on loopback and the mock vault of
// `test-fixtures/mock-vault.json`. Never deployed; `npm run build:mock` is for Playwright only.
import fixture from '../../test-fixtures/mock-vault.json';
import testExecutors from '../../test-fixtures/executors.json';
import type { Profile } from './types';

export const profile: Profile = {
  id: 'mock',
  network: 'testnet',
  appOrigin: 'http://localhost:4173',
  operatorUrl: fixture.operatorUrl,
  rpcUrls: fixture.rpcUrls,
  vaultAccount: fixture.vaultAccount,
  tokenAccount: fixture.tokenAccount,
  tokenSymbol: 'USDC',
  tokenDecimals: 6,
  explorerTxUrl: (hash) => `https://testnet.nearblocks.io/txns/${hash}`,
  faucetUrl: 'https://faucet.circle.com',
  walletIds: ['fake-wallet', 'intear-wallet', 'meteor-wallet'],
  clickBeforeSend: ['meteor-wallet', 'nymira-gated-test-wallet'],
  extraExecutors: testExecutors.wallets,
  walletConnectSrc: [],
  oneClick: {
    baseUrl: 'http://127.0.0.1:18547',
    // The fake 1Click signs with the key of seed [44; 32], as nymirad's test fixture does.
    signerKey: 'ed25519:AzXW61LgzhJTXN1so7rBR5auU2oCSzRyNEqFxPkZct3G',
    dryQuotes: true,
    // The mock build runs the Intents flow against the fake 1Click; testnet stays quote only.
    intentsPaths: true,
    intentsTwoStep: true,
    shortlist: [
      {
        label: 'USDC on Ethereum',
        assetId: 'nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near',
        dummyRefund: '0x0000000000000000000000000000000000000001',
      },
      {
        label: 'USDC on Solana',
        assetId: 'nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near',
        dummyRefund: '1nc1nerator11111111111111111111111111111111',
      },
    ],
  },
  pins: fixture.pins,
  gatewayUrl: 'http://127.0.0.1:18548',
  pccsUrl: 'http://127.0.0.1:18548',
  // The development setup's request key, served by the mock build only (vite.config.ts).
  provingKey: {
    url: '/setup/request.pk',
    sha256: 'fb5f0ec0908b29aa5a3ba75c5cefda70ca0bc1d68da031c4a21ef63c0806af87',
  },
  withdrawalKey: {
    url: '/setup/withdrawal.pk',
    sha256: 'c04e0368a85378c268b68c9c18764b0d1f86bac98fb21d69e6e80918ce2e486f',
  },
  testEvidence: true,
  // Shorter than a deployed build waits, so a wallet that never answers ends within a test.
  watch: { pollMs: 1000, graceMs: 6000, maxMs: 25_000 },
};
