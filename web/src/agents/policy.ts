// The two things the Agents screen has the owner sign, and the policy file they sign. Everything
// here is a pure function of typed fields, and its output has to match what `nymirad` reads
// (crates/nymirad/src/policy.rs and approvals.rs): the restricted YAML, the JSON message, the
// recipient tags and the signed file. A change on either side needs a change on the other.
import { b64encode } from '../crypto/bytes';
import { DEFAULT_BOUND, MODELS } from '../chat/models';

export const POLICY_KIND = 'nymira-policy';
export const APPROVAL_KIND = 'nymira-approval';
const U32_MAX = 4_294_967_295;

/** `agents::valid_name` in nymirad. */
export const AGENT_NAME = /^[a-z0-9_-]{1,32}$/;
/** A note commitment as nymirad writes it (`0x` and hex). It is a recipient part, never parsed. */
export const COMMITMENT = /^(0x)?[0-9a-fA-F]{1,128}$/;

export interface PolicyFields {
  models: string[];
  maxLeaseUsd: string;
  maxCallUsd: string;
  maxDailyUsd: string;
  confirmAboveUsd: string;
  rateLimitPerMin: string;
}

/** `amount` in policy.rs: at most 12 characters, digits and one dot, finite and above zero. */
const AMOUNT = /^[0-9][0-9.]{0,11}$/;
function amountError(label: string, text: string): string | null {
  const ok =
    AMOUNT.test(text) &&
    text.split('.').length <= 2 &&
    !text.endsWith('.') &&
    Number.isFinite(Number(text)) &&
    Number(text) > 0;
  return ok
    ? null
    : `${label}: a positive number like 0.25 (digits and one dot, at most 12 characters)`;
}

export type Built<T> = { ok: true; value: T } | { ok: false; errors: string[] };

export function buildPolicyYaml(f: PolicyFields): Built<string> {
  const errors: string[] = [];
  const known = new Set(MODELS.map((m) => m.id));
  if (f.models.length === 0) errors.push('Pick at least one model.');
  for (const [i, m] of f.models.entries()) {
    if (!known.has(m)) errors.push(`${m} is not a pinned model.`);
    if (f.models.indexOf(m) !== i) errors.push(`${m} is listed twice.`);
  }
  const amounts: [string, string][] = [
    ['Per lease limit', f.maxLeaseUsd],
    ['Per call limit', f.maxCallUsd],
    ['Per day limit', f.maxDailyUsd],
    ['Ask me above', f.confirmAboveUsd],
  ];
  for (const [label, text] of amounts) {
    const e = amountError(label, text);
    if (e) errors.push(e);
  }
  if (!amountError('', f.maxLeaseUsd) && Number(f.maxLeaseUsd) < Number(DEFAULT_BOUND) / 1e6) {
    errors.push(
      `Per lease limit: at least ${Number(DEFAULT_BOUND) / 1e6}, the smallest lease class.`,
    );
  }
  const rate = f.rateLimitPerMin;
  if (!/^[0-9]{1,10}$/.test(rate) || Number(rate) < 1 || Number(rate) > U32_MAX) {
    errors.push('Requests a minute: a whole number above 0.');
  }
  if (errors.length > 0) return { ok: false, errors };
  return {
    ok: true,
    value:
      `allowed_models:\n${f.models.map((m) => `  - ${m}\n`).join('')}` +
      `max_lease_usd: ${f.maxLeaseUsd}\nmax_call_usd: ${f.maxCallUsd}\n` +
      `max_daily_usd: ${f.maxDailyUsd}\nconfirm_above_usd: ${f.confirmAboveUsd}\n` +
      `rate_limit_per_min: ${rate}\nrequire_tee: true\n`,
  };
}

export function checkAgent(name: string, commitment: string): string[] {
  const errors: string[] = [];
  if (!AGENT_NAME.test(name)) errors.push('Agent name: 1 to 32 characters of a-z, 0-9, - and _.');
  if (!COMMITMENT.test(commitment)) {
    errors.push('Note commitment: hexadecimal, as `nymirad` shows it (for example 0x1a2b...).');
  }
  return errors;
}

export const policyRecipient = (agent: string, commitment: string) =>
  `nymirad:${agent}:${commitment}`;
export const approvalRecipient = (agent: string, commitment: string) =>
  `nymirad-approval:${agent}:${commitment}`;

export function policyMessage(version: number, yaml: string): Built<string> {
  if (!Number.isInteger(version) || version < 1 || version > U32_MAX) {
    return { ok: false, errors: ['Version: a whole number of 1 or more.'] };
  }
  return { ok: true, value: JSON.stringify({ kind: POLICY_KIND, version, policy: yaml }) };
}

export interface ApprovalFields {
  id: string;
  requestHash: string;
  amountMicro: string;
  nonce: string;
}

export function approvalMessage(f: ApprovalFields): Built<string> {
  const errors: string[] = [];
  if (!/^apv_[0-9a-f]{32}$/.test(f.id))
    errors.push('Approval ID: apv_ and 32 lowercase hex digits.');
  if (!/^[0-9a-f]{64}$/.test(f.requestHash)) errors.push('Request hash: 64 lowercase hex digits.');
  if (!/^[1-9][0-9]{0,38}$/.test(f.amountMicro)) {
    errors.push('Amount: whole micro-USD, no leading zero, above 0.');
  }
  if (!/^[0-9a-f]{64}$/.test(f.nonce)) errors.push('Nonce: 64 lowercase hex digits.');
  if (errors.length > 0) return { ok: false, errors };
  return {
    ok: true,
    value: JSON.stringify({
      kind: APPROVAL_KIND,
      id: f.id,
      request_hash: f.requestHash,
      amount_micro: f.amountMicro,
      nonce: f.nonce,
    }),
  };
}

export interface Signed {
  accountId: string;
  publicKey: string;
  /** base64 */
  signature: string;
  nonce: Uint8Array;
  message: string;
  recipient: string;
}

/** The file `nymirad agent policy` and `agent approve` read. Nothing else is in it. */
export const signedFile = (s: Signed): string =>
  JSON.stringify(
    {
      account_id: s.accountId,
      public_key: s.publicKey,
      signature: s.signature,
      nonce: b64encode(s.nonce),
      message: s.message,
      recipient: s.recipient,
    },
    null,
    2,
  ) + '\n';
