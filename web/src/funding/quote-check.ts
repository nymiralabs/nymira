// The checks on a 1Click quote response, the same ones nymirad runs (crates/nymirad/src/oneclick.rs):
// the manager's signature over the SDK's field selection, then the response against the request
// this page sent. A quote that fails either is never shown as payable.
import { ed25519 } from '@noble/curves/ed25519.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { base58Decode } from '../wallet/nep413';

type Json = Record<string, unknown>;

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

export function base58Encode(bytes: Uint8Array): string {
  const digits: number[] = [];
  for (const byte of bytes) {
    let carry = byte;
    for (let i = 0; i < digits.length; i++) {
      carry += (digits[i] as number) << 8;
      digits[i] = carry % 58;
      carry = Math.floor(carry / 58);
    }
    while (carry > 0) {
      digits.push(carry % 58);
      carry = Math.floor(carry / 58);
    }
  }
  let zeros = '';
  for (const byte of bytes) {
    if (byte !== 0) break;
    zeros += '1';
  }
  return (
    zeros +
    digits
      .reverse()
      .map((d) => ALPHABET[d])
      .join('')
  );
}

const REQUEST_FIELDS = [
  'dry',
  'swapType',
  'slippageTolerance',
  'originAsset',
  'depositType',
  'destinationAsset',
  'amount',
  'refundTo',
  'refundType',
  'recipient',
  'recipientType',
  'deadline',
];
const OPTIONAL_REQUEST_FIELDS = [
  'quoteWaitingTimeMs',
  'referral',
  'virtualChainRecipient',
  'virtualChainRefundRecipient',
  'customRecipientMsg',
];
const QUOTE_FIELDS = [
  'amountIn',
  'amountInFormatted',
  'amountInUsd',
  'minAmountIn',
  'amountOut',
  'amountOutFormatted',
  'amountOutUsd',
  'minAmountOut',
];
const LIVE_FIELDS = [
  'depositAddress',
  'depositMemo',
  'deadline',
  'timeWhenInactive',
  'timeEstimate',
  'refundFee',
  'withdrawFee',
];

function sorted(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sorted);
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Json)
        .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
        .map(([k, v]) => [k, sorted(v)]),
    );
  }
  return value;
}

/** The SDK's signed field selection, as stable JSON, SHA-256 and base58. */
export function quoteHash(response: Json): string {
  const request = (response.quoteRequest ?? {}) as Json;
  const quote = (response.quote ?? {}) as Json;
  const data: Json = {};
  for (const name of REQUEST_FIELDS) {
    if (!(name in request)) throw new Error(`missing quoteRequest.${name}`);
    data[name] = request[name];
  }
  for (const name of OPTIONAL_REQUEST_FIELDS) if (request[name]) data[name] = request[name];
  for (const name of QUOTE_FIELDS) {
    if (!(name in quote)) throw new Error(`missing quote.${name}`);
    data[name] = quote[name];
  }
  if (request.dry === false) {
    for (const name of LIVE_FIELDS) if (quote[name]) data[name] = quote[name];
  }
  if (typeof response.timestamp !== 'string') throw new Error('missing timestamp');
  data.timestamp = response.timestamp;
  return base58Encode(sha256(new TextEncoder().encode(JSON.stringify(sorted(data)))));
}

/** Throws a short reason when the signature is missing, malformed or not the key's. */
export function verifyQuoteSignature(response: Json, key: string): void {
  const signature = response.signature;
  if (typeof signature !== 'string') throw new Error('the quote has no signature');
  let ok = false;
  try {
    ok = ed25519.verify(
      base58Decode(signature.replace(/^ed25519:/, '')),
      new TextEncoder().encode(quoteHash(response)),
      base58Decode(key.replace(/^ed25519:/, '')),
      { zip215: false },
    );
  } catch {
    // A malformed signature or an unsigned field set is the same answer: not verified.
  }
  if (!ok) throw new Error('the quote signature does not verify');
}

/**
 * The response against the request this page sent: not expired, every request field echoed back
 * unchanged (recipient and `customRecipientMsg` included), and exactly the amount asked for.
 */
export function validateQuote(response: Json, sent: Json, now: number): void {
  const quote = (response.quote ?? {}) as Json;
  for (const value of [sent.deadline, quote.deadline]) {
    if (value === undefined) continue;
    const at = typeof value === 'string' ? Date.parse(value) : NaN;
    if (Number.isNaN(at)) throw new Error('the quote has an invalid deadline');
    if (at <= now) throw new Error('the quote expired: request a new quote before paying');
  }
  const echoed = (response.quoteRequest ?? {}) as Json;
  for (const [name, value] of Object.entries(sent)) {
    if (JSON.stringify(echoed[name]) !== JSON.stringify(value)) {
      throw new Error(`the quote changed the request field ${name}`);
    }
  }
  if (quote.amountOut !== sent.amount) {
    throw new Error('the quote amountOut is not the denomination plus the note fee');
  }
}

/** Two-step path: the quote pays the user's own account, never the vault, with no message. */
export function validateTwoStep(response: Json, account: string, vault: string): void {
  const request = (response.quoteRequest ?? {}) as Json;
  if (request.recipient === vault) {
    throw new Error('a two-step quote must not name the vault as recipient');
  }
  if (request.recipient !== account)
    throw new Error('the quote does not pay your own NEAR account');
  const message = request.customRecipientMsg;
  if (message !== undefined && message !== null && message !== '') {
    throw new Error('a two-step quote must carry no custom message');
  }
}

/** Signature first, then the request check; the first failure is the reason. */
export function checkQuote(response: Json, sent: Json, key: string, now: number): void {
  verifyQuoteSignature(response, key);
  validateQuote(response, sent, now);
}
