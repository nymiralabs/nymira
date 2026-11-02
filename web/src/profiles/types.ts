// One build profile is compiled into each bundle (`vite build --mode <profile>`). Network, vault,
// RPC endpoints, operator and the pins below are constants of that bundle: nothing in the URL,
// local storage or IndexedDB can change them. `src/profile.ts` re-exports the selected one.

export type NetworkName = 'testnet' | 'mainnet';
export type ProfileId = 'mock' | 'testnet' | 'mainnet';

/** A point on the operator's signature curve, as the vault's `config()` spells it. */
export interface PublicKeyPin {
  x: string;
  y: string;
}

/**
 * What `config()` returns, minus `tvl_cap` and `current_tvl` (those move by design). U128 and U64
 * values are decimal strings, as the contract serializes them.
 */
export interface VaultConfigPin {
  operator_pk: PublicKeyPin;
  clearance_pk: PublicKeyPin;
  operator_account: string;
  token: string;
  treasury: string;
  owner: string;
  chain_id: string;
  denominations: string[];
  note_fee: string;
  close_fee: string;
  bound_classes: string[];
  charge_step: string;
  min_charge: string;
  slot_rate: number;
  exit_delay_secs: number;
  note_ttl: number;
}

export interface AccessKeyPin {
  public_key: string;
  permission: unknown;
}

/** Everything the bundle knows about the one vault it talks to. */
export interface VaultPins {
  /** `view_account(vault).code_hash` (base58). */
  codeHash: string;
  /** `vk_hashes()`. */
  vkHashes: { request: string; withdrawal: string };
  config: VaultConfigPin;
  /** `view_access_key_list(vault)`: public keys and permissions, in any order. */
  accessKeys: AccessKeyPin[];
  /** The team's published note IDs, for the later privacy panel. Not checked against the chain. */
  teamNoteIds: number[];
}

export interface QuoteAsset {
  label: string;
  assetId: string;
  /** A syntactically valid, unowned refund address on the origin chain. */
  dummyRefund: string;
}

export interface Profile {
  id: ProfileId;
  network: NetworkName;
  /** The one origin this bundle runs from. The operator answers no other origin. */
  appOrigin: string;
  /** The operator's public API. Funding does not call it; chat does. */
  operatorUrl: string;
  rpcUrls: string[];
  vaultAccount: string;
  tokenAccount: string;
  tokenSymbol: string;
  tokenDecimals: number;
  /** `https://.../tx/<hash>` of the block explorer for a transaction. */
  explorerTxUrl: (hash: string) => string;
  /** Set on testnet only. */
  faucetUrl: string | null;
  /** Which NEAR Connect executors the wallet list offers (ids in `pins/executors.json`). */
  walletIds: string[];
  /** Executor pins that exist in this profile only (the mock profile's test wallet). */
  extraExecutors: Record<string, unknown>;
  /** Extra hosts the wallet frame may reach (its own CSP). Starting list, checked by hand. */
  walletConnectSrc: string[];
  oneClick: {
    baseUrl: string;
    /** Live dry quotes on the Fund screen (testnet and mock builds). */
    dryQuotes: boolean;
    /** Intents funding paths. Mainnet profile only. */
    intentsPaths: boolean;
    shortlist: QuoteAsset[];
  };
  /** `null` until a vault is deployed and pinned: funding then stays disabled. */
  pins: VaultPins | null;
}
