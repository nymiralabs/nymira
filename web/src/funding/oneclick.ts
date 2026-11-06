// Live 1Click `dry: true` quotes for a short shortlist of origin tokens, shown on the Fund screen of
// a testnet build. A dry quote prices a cross-chain payment and creates nothing: no deposit
// address, no order. These requests carry a dummy refund address and a placeholder recipient, never
// the note's commitment or the payout account.
//
// The cross-chain payment itself is not executed on testnet (1Click and Intents have no testnet)
// and nothing here says it was tested. The Intents paths belong to the mainnet profile.
import type { Profile, QuoteAsset } from '../profiles/types';
import { checkQuote } from './quote-check';

/** NEAR USDC on mainnet: the only destination 1Click can quote, also when this build is testnet. */
export const MAINNET_USDC_ASSET =
  'nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1';
/** An existing mainnet account; a dry quote never pays it, and it is never the vault or a user. */
export const PLACEHOLDER_RECIPIENT = 'nymira.near';
/** The commitment and payout a dry quote carries in its message: placeholders, never real values. */
export const placeholderMessage = () =>
  JSON.stringify({ commitment: `0x${'01'.repeat(32)}`, withdraw_to: PLACEHOLDER_RECIPIENT });
/** The keyless limit is 1 request per 5 seconds. */
export const MIN_GAP_MS = 5000;
export const CACHE_MS = 180_000;

export const dryQuotesEnabled = (p: Pick<Profile, 'oneClick'>) => p.oneClick.dryQuotes;
export const intentsPathsEnabled = (p: Pick<Profile, 'oneClick'>) => p.oneClick.intentsPaths;

export interface Quote {
  label: string;
  /** What the origin side would pay, in the origin token's units, as 1Click formats it. */
  amountInFormatted: string;
  timeEstimateSeconds: number | null;
}

export type QuoteOutcome = { label: string; quote: Quote } | { label: string; error: string };

export function buildRequest(p: {
  dry: boolean;
  assetId: string;
  amount: bigint;
  refund: string;
  recipient: string;
  /** Left out of the request on the two-step path: 1Click pays a plain transfer. */
  message?: string;
  now: number;
}): Record<string, unknown> {
  return {
    dry: p.dry,
    swapType: 'EXACT_OUTPUT',
    slippageTolerance: 100,
    originAsset: p.assetId,
    depositType: 'ORIGIN_CHAIN',
    destinationAsset: MAINNET_USDC_ASSET,
    amount: p.amount.toString(),
    refundTo: p.refund,
    refundType: 'ORIGIN_CHAIN',
    recipient: p.recipient,
    recipientType: 'DESTINATION_CHAIN',
    ...(p.message === undefined ? {} : { customRecipientMsg: p.message }),
    deadline: new Date(p.now + 3_600_000).toISOString(),
  };
}

/** The dry request: a dummy refund address and placeholder recipient and message. */
export const quoteBody = (asset: QuoteAsset, amount: bigint, now: number) =>
  buildRequest({
    dry: true,
    assetId: asset.assetId,
    amount,
    refund: asset.dummyRefund,
    recipient: PLACEHOLDER_RECIPIENT,
    message: placeholderMessage(),
    now,
  });

export class QuoteBook {
  private readonly cache = new Map<string, { at: number; outcome: QuoteOutcome }>();
  private lastRequestAt = 0;
  private queue: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly baseUrl: string,
    private readonly signerKey: string,
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
      const sent = quoteBody(asset, amount, this.now());
      const response = await this.fetchImpl(`${this.baseUrl}/v0/quote`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(sent),
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
      });
      if (!response.ok) return { label: asset.label, error: `1Click answered ${response.status}` };
      const body = (await response.json()) as {
        quote?: { amountInFormatted?: string; timeEstimate?: number };
      };
      // Only a signed quote that answers exactly this request counts as a source.
      try {
        checkQuote(body, sent, this.signerKey, this.now());
      } catch (error) {
        return { label: asset.label, error: error instanceof Error ? error.message : 'bad quote' };
      }
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
