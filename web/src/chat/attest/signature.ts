// What the badge says: how far an answer is vouched for, from the signature NEAR AI returned for
// the exact request and response bytes (`nearai-inference-sdk` `response.rs`, as `nymirad` uses it).
//
//  provider_tee  signed text `model:SHA256(request):SHA256(response)`, signer = the attested model
//  gateway       signed text `SHA256(request):SHA256(response)`, signer = the attested gateway
//
// A signature that is missing, malformed, over other bytes or by another key is `not_verified`,
// with the reason. Neither kind proves the gateway's TLS endpoint: a browser cannot bind it.
import { ed25519 } from '@noble/curves/ed25519.js';
import { sha256 } from '@noble/hashes/sha2.js';
import { fromHex, toHex } from '../../crypto/bytes';
import type { Attested } from './verify';

export type Trust =
  | { result: 'model_signed' }
  | { result: 'gateway_signed' }
  | { result: 'not_verified'; reason: string };

export interface CompletionSignature {
  signature_kind?: string;
  text?: string;
  signature?: string;
  signing_address?: string;
  signing_algo?: string;
}

const hash = (b: Uint8Array): string => toHex(sha256(b));

export function verifyCompletionSignature(
  attested: Attested,
  sig: CompletionSignature | null | undefined,
  model: string,
  request: Uint8Array,
  response: Uint8Array,
): Trust {
  const no = (reason: string): Trust => ({ result: 'not_verified', reason });
  if (!sig || typeof sig !== 'object') return no('no signature for this answer');
  // The SDK reads a bare `signature` as `provider_tee`.
  const kind = sig.signature_kind ?? (sig.signature ? 'provider_tee' : undefined);
  const gateway = kind === 'gateway';
  if (kind !== 'provider_tee' && !gateway) return no(`unknown signature kind ${String(kind)}`);
  const text = gateway
    ? `${hash(request)}:${hash(response)}`
    : `${model}:${hash(request)}:${hash(response)}`;
  if (sig.text !== text) return no('the signed text does not match the exchanged bytes');
  const signer = gateway ? attested.gateway : attested.model;
  if (sig.signing_algo !== 'ed25519' || (sig.signing_address ?? '').toLowerCase() !== signer.signingAddress) {
    return no('the signer is not the attested key');
  }
  let ok = false;
  try {
    ok = ed25519.verify(fromHex(sig.signature ?? ''), new TextEncoder().encode(text), fromHex(signer.signingAddress), {
      zip215: false,
    });
  } catch {
    ok = false;
  }
  if (!ok) return no('the signature does not verify');
  return { result: gateway ? 'gateway_signed' : 'model_signed' };
}

/** What the UI shows on the badge line. */
export function badgeLabel(t: Trust): string {
  switch (t.result) {
    case 'model_signed':
      return 'model-signed response';
    case 'gateway_signed':
      return 'gateway-signed response';
    case 'not_verified':
      return 'not verified';
  }
}

/** Several exchanges on one lease: the weakest result counts. */
export function weakest(items: Trust[]): Trust {
  if (items.length === 0) return { result: 'not_verified', reason: 'no chat on this lease' };
  const bad = items.find((t) => t.result === 'not_verified');
  if (bad) return bad;
  return items.some((t) => t.result === 'gateway_signed')
    ? { result: 'gateway_signed' }
    : { result: 'model_signed' };
}
