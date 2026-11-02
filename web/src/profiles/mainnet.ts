// The later launch profile. There is no public mainnet vault during the hackathon (ADR-0019), so
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
  extraExecutors: {},
  walletConnectSrc: [],
  oneClick: {
    baseUrl: 'https://1click.chaindefuser.com',
    // The dry-quote listing is a testnet feature; on mainnet the Intents paths themselves are on.
    dryQuotes: false,
    intentsPaths: true,
    shortlist: [],
  },
  pins: null,
  gatewayUrl: 'https://cloud-api.near.ai',
  pccsUrl: 'https://pccs.phala.network',
  provingKey: null,
  testEvidence: false,
};
