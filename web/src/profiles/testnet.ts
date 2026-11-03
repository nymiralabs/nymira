// The hackathon deployment (ADR-0019): NEAR testnet, Circle's testnet USDC, test funds only.
//
// `pins` stays null until the judge vault is deployed. Fill it with
// `npm run pin-vault -- --network testnet --vault <account>`, review the diff and commit it; until
// then the Fund screen is disabled and says so.
import type { Profile } from './types';

export const profile: Profile = {
  id: 'testnet',
  network: 'testnet',
  appOrigin: 'https://nymira.pages.dev',
  // Set by the testnet launch ticket, together with the operator's CORS origin.
  operatorUrl: '',
  rpcUrls: ['https://rpc.testnet.near.org', 'https://rpc.testnet.fastnear.com'],
  vaultAccount: 'vault.nymira.testnet',
  tokenAccount: '3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af',
  tokenSymbol: 'USDC',
  tokenDecimals: 6,
  explorerTxUrl: (hash) => `https://testnet.nearblocks.io/txns/${hash}`,
  faucetUrl: 'https://faucet.circle.com',
  walletIds: ['meteor-wallet', 'intear-wallet'],
  extraExecutors: {},
  walletConnectSrc: [],
  oneClick: {
    baseUrl: 'https://1click.chaindefuser.com',
    dryQuotes: true,
    intentsPaths: false,
    shortlist: [
      {
        label: 'USDC on Ethereum',
        assetId: 'nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near',
        dummyRefund: '0x0000000000000000000000000000000000000001',
      },
      {
        label: 'USDC on Base',
        assetId: 'nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near',
        dummyRefund: '0x0000000000000000000000000000000000000001',
      },
      {
        label: 'USDC on Arbitrum',
        assetId: 'nep141:arb-0xaf88d065e77c8cc2239327c5edb3a432268e5831.omft.near',
        dummyRefund: '0x0000000000000000000000000000000000000001',
      },
      {
        label: 'USDC on Solana',
        assetId: 'nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near',
        dummyRefund: '1nc1nerator11111111111111111111111111111111',
      },
    ],
  },
  pins: null,
  gatewayUrl: 'https://cloud-api.near.ai',
  pccsUrl: 'https://pccs.phala.network',
  // Set with the published setup (the ceremony's `mpc-1` or Nymira's fallback key) and its hash.
  provingKey: null,
  withdrawalKey: null,
  testEvidence: false,
};
