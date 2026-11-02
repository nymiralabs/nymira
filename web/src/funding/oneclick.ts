// Live 1Click `dry: true` quotes for a short shortlist of origin tokens, shown on the Fund screen of
// a testnet build. A dry quote prices a cross-chain payment and creates nothing: no deposit
// address, no order. These requests carry a dummy refund address and a placeholder recipient, never
// the note's commitment or the payout account.
//
// The cross-chain payment itself is not executed on testnet (1Click and Intents have no testnet)
// and nothing here says it was tested. The Intents paths belong to the mainnet profile.
import type { Profile, QuoteAsset } from '../profiles/types';

/** NEAR USDC on mainnet: the only destination 1Click can quote, also when this build is testnet. */
export const MAINNET_USDC_ASSET =
  'nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1';
/** A valid NEAR account that is nobody's; the dry quote never pays it. */
export const PLACEHOLDER_RECIPIENT = 'example.near';
/** The keyless limit is 1 request per 5 seconds. */
export const MIN_GAP_MS = 5000;
export const CACHE_MS = 60_000;

export const dryQuotesEnabled = (p: Pick<Profile, 'oneClick'>) => p.oneClick.dryQuotes;
export const intentsPathsEnabled = (p: Pick<Profile, 'oneClick'>) => p.oneClick.intentsPaths;

export interface Quote {
  label: string;
  /** What the origin side would pay, in the origin token's units, as 1Click formats it. */
  amountInFormatted: string;
  timeEstimateSeconds: number | null;
}

export type QuoteOutcome = { label: string; quote: Quote } | { label: string; error: string };

export function quoteBody(asset: QuoteAsset, amount: bigint, now: number): Record<string, unknown> {
  return {
    dry: true,
    swapType: 'EXACT_OUTPUT',
    slippageTolerance: 100,
    originAsset: asset.assetId,
    depositType: 'ORIGIN_CHAIN',
    destinationAsset: MAINNET_USDC_ASSET,
    amount: amount.toString(),
    refundTo: asset.dummyRefund,
    refundType: 'ORIGIN_CHAIN',
    recipient: PLACEHOLDER_RECIPIENT,
    recipientType: 'DESTINATION_CHAIN',
    deadline: new Date(now + 3_600_000).toISOString(),
  };
}

export class QuoteBook {
  private readonly cache = new Map<string, { at: number; outcome: QuoteOutcome }>();
  private lastRequestAt = 0;
  private queue: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly baseUrl: string,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
    private readonly now: () => number = Date.now,
    private readonly sleep: (ms: number) => Promise<void> = (ms) =>
      new Promise((r) => setTimeout(r, ms)),
  ) {}

  /** One quote; answered from the cache for a minute, and requests go out at most one per 5 s. */
  quote(asset: QuoteAsset, amount: bigint): Promise<QuoteOutcome> {
    const key = `${asset.assetId}:${amount}`;
    const hit = this.cache.get(key);
    if (hit && this.now() - hit.at < CACHE_MS) return Promise.resolve(hit.outcome);
    const run = this.queue.then(async () => {
      const wait = this.lastRequestAt + MIN_GAP_MS - this.now();
      if (wait > 0) await this.sleep(wait);
      this.lastRequestAt = this.now();
      const outcome = await this.request(asset, amount);
      this.cache.set(key, { at: this.now(), outcome });
      return outcome;
    });
    this.queue = run.catch(() => undefined);
    return run;
  }

  private async request(asset: QuoteAsset, amount: bigint): Promise<QuoteOutcome> {
    try {
      const response = await this.fetchImpl(`${this.baseUrl}/v0/quote`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(quoteBody(asset, amount, this.now())),
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
      });
      if (!response.ok) return { label: asset.label, error: `1Click answered ${response.status}` };
      const body = (await response.json()) as {
        quote?: { amountInFormatted?: string; timeEstimate?: number };
      };
      const formatted = body.quote?.amountInFormatted;
      if (typeof formatted !== 'string')
        return { label: asset.label, error: 'no quote in the answer' };
      return {
        label: asset.label,
        quote: {
          label: asset.label,
          amountInFormatted: formatted,
          timeEstimateSeconds: body.quote?.timeEstimate ?? null,
        },
      };
    } catch {
      return { label: asset.label, error: 'could not reach 1Click' };
    }
  }
}
