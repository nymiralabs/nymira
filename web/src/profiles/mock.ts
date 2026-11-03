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
  extraExecutors: testExecutors.wallets,
  walletConnectSrc: [],
  oneClick: {
    baseUrl: 'http://127.0.0.1:18547',
    dryQuotes: true,
    intentsPaths: false,
    shortlist: [
      {
        label: 'USDC on Ethereum',
        assetId: 'nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near',
        dummyRefund: '0x0000000000000000000000000000000000000001',
      },
      {
        label: 'USDC on Solana',
        assetId: 'nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near',
        dummyRefund: '11111111111111111111111111111111',
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
  testEvidence: true,
};
