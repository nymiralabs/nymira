// The chat's dependencies in the page: the signed policy, the status-signature check and the
// quote verifier (Intel's, through WASM and a PCCS; the test one in the mock build only), plus the
// proving key warm-up in the worker.
import initChecks, {
  fetch_collateral,
  status_signature_valid,
  verify_tdx_quote,
} from '../wasm/web-checks/nymira_web_wasm.js';
import policyJson from '../../pins/attestation-policy.json';
import signerPk from '../../pins/policy-signer.pub?raw';
import { PROFILE } from '../profile';
import { callWorker } from '../worker/client';
import { rpc } from '../ui/services';
import { liveQuoteVerifier } from './attest/intel';
import { loadPolicy, type Policy } from './attest/policy';
import type { QuoteVerifier } from './attest/verify';
import type { SignatureCheck } from './gate';
import { OperatorClient } from './operator';
import type { Deps } from './session';

/** The oldest policy this build accepts. Raise it with each signed release. */
export const MIN_POLICY_VERSION = 1;

let ready: Promise<unknown> | null = null;
const checksReady = () => (ready ??= initChecks());

export const signatureCheck: SignatureCheck = (pk, nonce, signatureJson) =>
  status_signature_valid(pk.x, pk.y, nonce.nonce, nonce.host, BigInt(nonce.ts), signatureJson);

async function quoteVerifier(): Promise<QuoteVerifier> {
  if (import.meta.env.MODE === 'mock') {
    return (await import('./attest/testquote')).testQuote;
  }
  return liveQuoteVerifier({ fetch_collateral, verify_tdx_quote }, PROFILE.pccsUrl);
}

export function loadSignedPolicy(): Policy {
  return loadPolicy(policyJson, signerPk, MIN_POLICY_VERSION);
}

export async function chatDeps(getKey: () => CryptoKey): Promise<Deps> {
  await checksReady();
  return {
    profile: PROFILE,
    rpc,
    operator: new OperatorClient(PROFILE.operatorUrl),
    policy: loadSignedPolicy(),
    quote: await quoteVerifier(),
    signatureCheck,
    getKey,
  };
}

export type ProverState =
  | { status: 'off'; reason: string }
  | { status: 'loading' }
  | { status: 'ready'; bytes: number; ms: number }
  | { status: 'failed'; reason: string };

let prover: Promise<ProverState> | null = null;

/** Loads the request proving key into the worker and checks its SHA-256; once per page. */
export function warmProver(): Promise<ProverState> {
  const key = PROFILE.provingKey;
  if (!key) {
    return Promise.resolve({
      status: 'off',
      reason: 'The proving key for this deployment is not published yet.',
    });
  }
  return (prover ??= callWorker({ op: 'loadProver', url: key.url, sha256: key.sha256 }).then(
    (r): ProverState => ({ status: 'ready', bytes: r.bytes, ms: r.ms }),
    (e: unknown): ProverState => ({
      status: 'failed',
      reason: e instanceof Error ? e.message : String(e),
    }),
  ));
}
