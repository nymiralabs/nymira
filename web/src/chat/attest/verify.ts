// Verifying NEAR AI's attestation evidence in the browser: the checks of the NEAR AI SDK
// (`verify_model_attestation`, `verify_gateway_attestation`) as `nymirad` runs them, plus the
// signed policy's deployment list.
//
// What a pass establishes:
//  - the Intel TDX quote verifies against Intel's root with fresh collateral (the `quote`
//    verifier; in the app it is dcap-qvl compiled to WASM) and its TCB status is UpToDate;
//  - the debug flag is off and the nonce we sent is in the quote's report data, next to the
//    signing address, so the key we encrypt to is bound to this quote and this request;
//  - the dstack event log replays to the quoted RTMR3 and the compose file hashes to `mr_config_id`;
//  - the measured deployment (`mr_td`, `rtmr0-2`, compose and OS image hashes) is one entry of the
//    signed policy;
//  - for a model, the signing key we encrypt to equals the attested signer.
//
// What it does not establish (the result says so, and the UI shows it):
//  - NVIDIA GPU evidence: NRAS refuses cross-origin requests, so a browser cannot ask it. Only the
//    payload's nonce is checked (`gpu: 'nonce_only'`); `nymirad` verifies the evidence itself.
//  - `mr_seam`, `xfam` and the TD attributes other than the debug bit (the quote's signature
//    covers them; the policy does not name them).
//  - The gateway's TLS certificate: a browser cannot read or pin it.
import { sha256, sha384 } from '@noble/hashes/sha2.js';
import { fromHex, toHex } from '../../crypto/bytes';
import { STRICT_TCB, type Deployment, type Policy } from './policy';

/** What a verified quote carries; values are lower-case hex (`QuoteFacts` of nymira-web-wasm). */
export interface QuoteFacts {
  status: string;
  advisory_ids: string[];
  debug: boolean;
  mr_td: string;
  rtmr0: string;
  rtmr1: string;
  rtmr2: string;
  rtmr3: string;
  mr_config_id: string;
  report_data: string;
}

export interface QuoteVerifier {
  verify(quoteHex: string): Promise<QuoteFacts>;
}

export interface Evidence {
  request_nonce: string;
  signing_address: string;
  signing_algo: string;
  intel_quote: string;
  event_log: unknown;
  report_data?: string;
  info?: { tcb_info?: { app_compose?: string } };
  signing_public_key?: string;
  nvidia_payload?: string;
}

export class AttestationError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = 'AttestationError';
  }
}

const fail = (code: string, message: string): never => {
  throw new AttestationError(code, message);
};

export interface Verified {
  role: 'model' | 'gateway';
  /** The signer's key as the report gives it (hex). For a model this is the key chats go to. */
  signingKey: string;
  signingAddress: string;
  tcbStatus: string;
  advisoryIds: string[];
  composeHash: string;
  osImageHash: string;
  /** `nonce_only`: the GPU payload's nonce matched; NVIDIA's verdict was not obtained. */
  gpu: 'nonce_only' | 'none';
}

const norm = (h: string): string => (h.startsWith('0x') ? h.slice(2) : h).toLowerCase();

const hexBytes = (value: string, bytes: number, what: string): Uint8Array => {
  const v = norm(value);
  if (v.length !== bytes * 2 || !/^[0-9a-f]+$/.test(v)) {
    fail('invalid_input', `${what} must be ${bytes} bytes of hex`);
  }
  return fromHex(v);
};

const DSTACK_RUNTIME_EVENT = 0x0800_0001;

interface Entry {
  imr: number;
  event_type?: number;
  digest: string;
  event?: string;
  event_payload?: string;
}

function eventDigest(e: Entry, at: string): Uint8Array {
  if (e.event_type === DSTACK_RUNTIME_EVENT) {
    const payload = norm(e.event_payload ?? '');
    if (payload.length % 2 !== 0 || !/^[0-9a-f]*$/.test(payload)) {
      fail('event_log_invalid', `${at}.event_payload is not hex`);
    }
    const head = new Uint8Array(4);
    new DataView(head.buffer).setUint32(0, DSTACK_RUNTIME_EVENT, true);
    const enc = new TextEncoder();
    const computed = sha384(
      new Uint8Array([...head, 0x3a, ...enc.encode(e.event ?? ''), 0x3a, ...fromHex(payload)]),
    );
    if (e.digest && toHex(computed) !== norm(e.digest)) {
      fail('event_log_invalid', `${at}.digest does not match the event`);
    }
    return computed;
  }
  const digest = norm(e.digest ?? '');
  if (digest.length !== 96 || !/^[0-9a-f]+$/.test(digest)) {
    fail('event_log_invalid', `${at}.digest is not a 48-byte hex string`);
  }
  return fromHex(digest);
}

/** Replays RTMR3 from dstack's event log; returns the compose and OS image hashes it recorded. */
export function replayRtmr3(
  eventLog: unknown,
  quotedRtmr3: string,
): { composeHash?: string; osImageHash?: string } {
  let list: unknown = eventLog;
  if (typeof list === 'string') {
    try {
      list = JSON.parse(list);
    } catch {
      return fail('event_log_invalid', 'the event log is not JSON');
    }
  }
  if (!Array.isArray(list)) return fail('event_log_invalid', 'the event log is not a list');
  let replayed = new Uint8Array(48);
  let count = 0;
  const out: { composeHash?: string; osImageHash?: string } = {};
  list.forEach((raw, i) => {
    const e = raw as Entry;
    if (typeof e !== 'object' || e === null || typeof e.imr !== 'number') {
      fail('event_log_invalid', `event_log[${i}] is malformed`);
    }
    if (e.imr !== 3) return;
    count += 1;
    replayed = sha384(new Uint8Array([...replayed, ...eventDigest(e, `event_log[${i}]`)]));
    if (e.event_type === DSTACK_RUNTIME_EVENT) {
      if (e.event === 'os-image-hash') out.osImageHash = norm(e.event_payload ?? '');
      if (e.event === 'compose-hash') out.composeHash = norm(e.event_payload ?? '');
    }
  });
  if (count === 0) fail('rtmr3_mismatch', 'the event log has no RTMR3 events');
  if (toHex(replayed) !== norm(quotedRtmr3)) {
    fail('rtmr3_mismatch', 'the event log does not replay to the quoted RTMR3');
  }
  return out;
}

const equalBytes = (a: Uint8Array, b: Uint8Array): boolean =>
  a.length === b.length && a.every((x, i) => x === b[i]);

function matchDeployments(policy: Policy, role: 'model' | 'gateway', f: QuoteFacts): Deployment[] {
  const same = (d: Deployment) =>
    d.mr_td === f.mr_td && d.rtmr0 === f.rtmr0 && d.rtmr1 === f.rtmr1 && d.rtmr2 === f.rtmr2;
  return policy.deployments.filter((d) => d.role === role && same(d));
}

/**
 * Verifies one report entry for `role` under `nonce`. Throws `AttestationError` with a stable
 * `code` on the first failed check.
 */
export async function verifyEvidence(
  role: 'model' | 'gateway',
  evidence: Evidence,
  nonce: string,
  policy: Policy,
  quote: QuoteVerifier,
): Promise<Verified> {
  const expected = hexBytes(nonce, 32, 'the nonce');
  if (!equalBytes(hexBytes(evidence.request_nonce, 32, 'the report nonce'), expected)) {
    fail('nonce_mismatch', 'the report answers another nonce');
  }
  if (evidence.signing_algo !== 'ed25519') fail('unsupported_algo', 'only Ed25519 signers are accepted');
  const address = hexBytes(evidence.signing_address, 32, 'the signing address');
  const facts = await quote.verify(evidence.intel_quote);

  if (evidence.report_data !== undefined && norm(evidence.report_data) !== facts.report_data) {
    fail('report_data_mismatch', 'the advertised report data differs from the quote’s');
  }
  if (facts.debug) fail('debug_enabled', 'the enclave runs in debug mode');
  if (!policy.accepted_tcb_statuses.includes(facts.status) || facts.status !== STRICT_TCB) {
    fail('tcb_status', `the TCB status is ${facts.status}; only ${STRICT_TCB} is accepted`);
  }
  const rd = fromHex(facts.report_data);
  if (rd.length !== 64) fail('report_data_invalid', 'the quote’s report data is not 64 bytes');
  if (!equalBytes(rd.subarray(32), expected)) {
    fail('nonce_mismatch', 'the quote does not carry the nonce that was sent');
  }
  if (!equalBytes(rd.subarray(0, 32), address)) {
    fail('signer_binding', 'the quote does not bind the signing address');
  }

  const runtime = replayRtmr3(evidence.event_log, facts.rtmr3);
  const compose = evidence.info?.tcb_info?.app_compose;
  if (typeof compose !== 'string') fail('compose_missing', 'the report has no app compose file');
  const cfg = fromHex(facts.mr_config_id);
  if (cfg.length < 33 || cfg[0] !== 1) fail('mr_config_id', 'the quote’s config id is not a compose hash');
  if (!equalBytes(cfg.subarray(1, 33), sha256(new TextEncoder().encode(compose as string)))) {
    fail('compose_mismatch', 'the compose file does not hash to the quote’s config id');
  }
  if (!runtime.composeHash || !runtime.osImageHash) {
    fail('deployment_rejected', 'the event log lacks the compose or OS image hash');
  }

  const matched = matchDeployments(policy, role, facts);
  if (matched.length === 0) {
    fail('measurement_not_accepted', 'the measured deployment is not in the signed policy');
  }
  if (!matched.some((d) => d.compose_hashes.includes(runtime.composeHash!) && d.os_image_hash === runtime.osImageHash)) {
    fail('deployment_rejected', 'the compose or OS image hash is not in the signed policy');
  }

  let gpu: Verified['gpu'] = 'none';
  if (role === 'model') {
    if (evidence.nvidia_payload === undefined) fail('gpu_required', 'the model report has no GPU evidence');
    let parsed: { nonce?: unknown };
    try {
      parsed = JSON.parse(evidence.nvidia_payload as string) as { nonce?: unknown };
    } catch {
      return fail('gpu_payload_invalid', 'the GPU payload is not JSON');
    }
    if (typeof parsed.nonce !== 'string' || !equalBytes(hexBytes(parsed.nonce, 32, 'the GPU nonce'), expected)) {
      fail('nonce_mismatch', 'the GPU payload answers another nonce');
    }
    gpu = 'nonce_only';
    const key = evidence.signing_public_key;
    if (key === undefined || !equalBytes(hexBytes(key, 32, 'the model key'), address)) {
      fail('model_key_mismatch', 'the model key is not the attested signing key');
    }
  }
  return {
    role,
    signingKey: toHex(address),
    signingAddress: toHex(address),
    tcbStatus: facts.status,
    advisoryIds: facts.advisory_ids,
    composeHash: runtime.composeHash,
    osImageHash: runtime.osImageHash,
    gpu,
  };
}

export interface ModelReport {
  gateway_attestation?: Evidence;
  model_attestations?: (Evidence & { model_name?: string })[];
}

export interface Attested {
  model: Verified;
  gateway: Verified;
}

/**
 * The model's key and the gateway's signer from a model report (`modelReport`, fetched under
 * `modelNonce`) and a gateway report (`gatewayReport`, fetched under `gatewayNonce`).
 */
export async function verifyReports(
  modelId: string,
  modelReport: ModelReport,
  modelNonce: string,
  gatewayReport: ModelReport,
  gatewayNonce: string,
  policy: Policy,
  quote: QuoteVerifier,
): Promise<Attested> {
  const candidates = (modelReport.model_attestations ?? []).filter(
    (e) => e.model_name === undefined || e.model_name === modelId,
  );
  let last: AttestationError | null = null;
  let model: Verified | null = null;
  for (const c of candidates) {
    if (!c.signing_public_key || c.signing_algo !== 'ed25519') continue;
    try {
      model = await verifyEvidence('model', c, modelNonce, policy, quote);
      break;
    } catch (e) {
      if (!(e instanceof AttestationError)) throw e;
      last = e;
    }
  }
  if (!model) {
    throw last ?? new AttestationError('no_model_attestation', 'the report holds no attestation for the model');
  }
  if (!gatewayReport.gateway_attestation) {
    fail('no_gateway_attestation', 'the gateway report has no gateway attestation');
  }
  const gateway = await verifyEvidence('gateway', gatewayReport.gateway_attestation!, gatewayNonce, policy, quote);
  return { model, gateway };
}
