// The later launch profile. There is no public mainnet vault during the hackathon, so
// `pins` is null and the Fund screen stays disabled. It stays in the code so the mainnet build,
// its CI checks and its constants exist when the launch comes.
import type { Profile } from './types';

export const profile: Profile = {
  id: 'mainnet',
  network: 'mainnet',
  appOrigin: 'https://nymira.pages.dev',
  operatorUrl: '',
  rpcUrls: ['https://free.rpc.fastnear.com', 'https://rpc.mainnet.near.org'],
  vaultAccount: 'vault.nymira.near',
  tokenAccount: '17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1',
  tokenSymbol: 'USDC',
  tokenDecimals: 6,
  explorerTxUrl: (hash) => `https://nearblocks.io/txns/${hash}`,
  faucetUrl: null,
  walletIds: ['meteor-wallet', 'intear-wallet'],
  clickBeforeSend: ['meteor-wallet'],
  extraExecutors: {},
  walletConnectSrc: [],
  oneClick: {
    baseUrl: 'https://1click.chaindefuser.com',
    // 1Click's manager key; a rotation needs a new release of this app.
    signerKey: 'ed25519:reYaWhvwu8Jzo3WUM3zhn6VrhuMEF4eADL17qtRVifc',
    // The dry-quote listing is a testnet feature; on mainnet one Intents path is on. `two-step`
    // by decision (issue 50): 1Click has given no written answer and the mainnet probe has not run.
    dryQuotes: false,
    intentsPath: 'two-step',
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
  provingKey: null,
  withdrawalKey: null,
  testEvidence: false,
};
