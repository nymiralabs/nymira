// The hackathon deployment: NEAR testnet, Circle's testnet USDC, test funds only.
//
// `pins` is the final vault, read with `npm run pin-vault -- --network testnet --vault
// vault.nymira.testnet` and reviewed against the deployment record.
import type { Profile } from './types';

export const profile: Profile = {
  id: 'testnet',
  network: 'testnet',
  appOrigin: 'https://nymira.pages.dev',
  // The operator's public Funnel URL; its `host` and CORS `web_origin` must match this and `appOrigin`.
  operatorUrl: 'https://nymira-operator.tail0484c4.ts.net',
  rpcUrls: ['https://rpc.testnet.fastnear.com', 'https://rpc.testnet.near.org'],
  vaultAccount: 'vault.nymira.testnet',
  tokenAccount: '3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af',
  tokenSymbol: 'USDC',
  tokenDecimals: 6,
  explorerTxUrl: (hash) => `https://testnet.nearblocks.io/txns/${hash}`,
  faucetUrl: 'https://faucet.circle.com',
  walletIds: ['meteor-wallet', 'intear-wallet'],
  clickBeforeSend: ['meteor-wallet'],
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
  pins: {
    codeHash: '71QXP9FT4eiRcaqjZtsHi7mXMCAexXjCFgQ338p8tur4',
    vkHashes: {
      request: '0xf3092e0d550f696a6507f279862afb43d0c22663de56f1b47160b5db6fc6fa1b',
      withdrawal: '0x3469ebb3245101549343dbe5939427d3dc7706c314f281f9b5935416fc720c8d',
    },
    config: {
      operator_pk: {
        x: '0x2ab6e2f4c85c60832e5fecaa0d88ea9f2b55cbff4261e9b6be5853afc09d812b',
        y: '0x2d8216aefd1ad4c183ffcb8d2238f55b3779baee9dbb938cf10842641cbb7bc1',
      },
      clearance_pk: {
        x: '0x2a84bf617974bb652505359591945b716533cf7d322cfa4f184100398fdc8b03',
        y: '0x06bbae77669b94f3da01e7378dab520adbfb17fc1393c67e7cb94b4eae8adcac',
      },
      operator_account: 'operator.nymira.testnet',
      token: '3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af',
      treasury: 'treasury.nymira.testnet',
      owner: 'owner.nymira.testnet',
      chain_id: '7718972759422244814',
      denominations: ['1000000', '5000000'],
      note_fee: '50000',
      close_fee: '30000',
      bound_classes: ['100000', '500000'],
      charge_step: '1000',
      min_charge: '2000',
      slot_rate: 1000,
      exit_delay_secs: 604800,
      note_ttl: 2592000,
    },
    accessKeys: [
      {
        public_key: 'ed25519:DJc2C6WWveyUY1VvLEdUPjBggzKrMuEwx7GGp1BLHMze',
        permission: 'FullAccess',
      },
    ],
    teamNoteIds: [],
  },
  gatewayUrl: 'https://cloud-api.near.ai',
  pccsUrl: 'https://pccs.phala.network',
  // The ceremony's final keys (`mpc-1`), served from the signed bundle under `setup/`; the hashes
  // equal the ones `nymirad` compiles in (`scripts/check-pins.sh`).
  provingKey: {
    url: '/setup/request.pk',
    sha256: 'b5cdb12000cf51cf566375aadbc2a27cf875438f646f751fa857371cd6cb7022',
  },
  withdrawalKey: {
    url: '/setup/withdrawal.pk',
    sha256: '89e03ac8f9d6e90d189e9a3eb6b0c9c0126e1b121e78955a11f0f70a44d0161d',
  },
  testEvidence: false,
};
