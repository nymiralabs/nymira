// `attestation-policy.json`: the signed list of NEAR AI deployments this app trusts, with strict
// TCB. Same file, same signed text and same checks as `nymirad/src/attest.rs` (`Policy::load`):
// the web copy in `web/pins/` is compared byte for byte with the one `nymirad` ships.
import { ed25519 } from '@noble/curves/ed25519.js';
import { fromHex } from '../../crypto/bytes';

/**
 * The single `compose_hashes` entry of a `gateway` deployment that accepts any compose hash. NEAR
 * AI redeploys the gateway often and each deployment changes its compose file, so pinning one
 * hash breaks chat after every redeploy. Everything else about the entry stays pinned. The
 * `model` role keeps exact hashes: plaintext prompts are decrypted there.
 */
export const ANY_COMPOSE = '*';

export interface Deployment {
  role: 'model' | 'gateway';
  /** Exact hashes, or `[ANY_COMPOSE]` for a gateway. */
  compose_hashes: string[];
  os_image_hash: string;
  mr_td: string;
  rtmr0: string;
  rtmr1: string;
  rtmr2: string;
}

/** A pinned NVIDIA certificate: the SHA-256 of its DER form, lower-case hex. */
export interface NvidiaPin {
  name: string;
  sha256: string;
  official_source_verified: boolean;
}

export interface Policy {
  version: number;
  attestation_root_pk: string;
  accepted_measurements: string[];
  deployments: Deployment[];
  accepted_tcb_statuses: string[];
  /** Absent or empty in a policy signed before the NVIDIA chain check existed. */
  nvidia_pins?: NvidiaPin[];
  signature: string;
}

/** The only TCB status a policy may accept. `OutOfDate` and every other status are refused. */
export const STRICT_TCB = 'UpToDate';

export class PolicyError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'PolicyError';
  }
}

/** Whether `d` takes a VM with this compose hash. */
export function acceptsCompose(d: Deployment, hash: string): boolean {
  return isComposeWildcard(d) || d.compose_hashes.includes(hash);
}

/** A gateway entry that accepts any compose hash; `loadPolicy` refuses the sentinel elsewhere. */
export function isComposeWildcard(d: Deployment): boolean {
  return (
    d.role === 'gateway' && d.compose_hashes.length === 1 && d.compose_hashes[0] === ANY_COMPOSE
  );
}

export function signedText(p: Policy): string {
  const deployments = p.deployments
    .map((d) =>
      [
        d.role,
        d.compose_hashes.join('+'),
        d.os_image_hash,
        d.mr_td,
        d.rtmr0,
        d.rtmr1,
        d.rtmr2,
      ].join(','),
    )
    .join(';');
  const base = [
    p.version,
    p.attestation_root_pk,
    p.accepted_measurements.join(','),
    p.accepted_tcb_statuses.join(','),
    deployments,
  ];
  const pins = p.nvidia_pins ?? [];
  // No pins: the text signed before pins existed. Pins: the v3 text, as `nymirad` signs it.
  if (pins.length === 0) return ['nymira/attestation-policy/v2', ...base].join('|');
  const pinned = pins
    .map((n) => [n.name, n.sha256, n.official_source_verified ? 'true' : 'false'].join(','))
    .join(';');
  return ['nymira/attestation-policy/v3', ...base, pinned].join('|');
}

const isHex = (v: unknown, bytes: number): boolean =>
  typeof v === 'string' && v.length === bytes * 2 && /^[0-9a-f]+$/.test(v);

/** Parses `json` and checks it against the signer key and the minimum version built into the app. */
export function loadPolicy(json: unknown, signerPkHex: string, minVersion: number): Policy {
  const p = json as Policy;
  if (typeof p !== 'object' || p === null || typeof p.version !== 'number') {
    throw new PolicyError('the policy file is not valid');
  }
  const deployments = Array.isArray(p.deployments) ? p.deployments : [];
  const policy: Policy = { ...p, deployments };
  let ok = false;
  try {
    ok = ed25519.verify(
      fromHex(policy.signature),
      new TextEncoder().encode(signedText(policy)),
      fromHex(signerPkHex.trim()),
      { zip215: false },
    );
  } catch {
    ok = false;
  }
  if (!ok) throw new PolicyError('the policy signature does not verify');
  if (policy.version < minVersion) {
    throw new PolicyError(
      `the policy is version ${policy.version}; this build needs at least ${minVersion}`,
    );
  }
  const lenient = policy.accepted_tcb_statuses.find((s) => s !== STRICT_TCB);
  if (lenient !== undefined) {
    throw new PolicyError(
      `the policy accepts TCB status ${lenient}, which a strict policy never does`,
    );
  }
  if (
    (policy.accepted_measurements.length === 0 && deployments.length === 0) ||
    policy.accepted_tcb_statuses.length === 0
  ) {
    throw new PolicyError('the policy accepts no measurement or deployment');
  }
  for (const n of policy.nvidia_pins ?? []) {
    const bad =
      typeof n.name !== 'string' ||
      n.name.length === 0 ||
      /[,;|]/.test(n.name) ||
      !isHex(n.sha256, 32) ||
      typeof n.official_source_verified !== 'boolean';
    if (bad) throw new PolicyError('the policy lists an NVIDIA pin with a malformed value');
  }
  for (const d of deployments) {
    const bad =
      !(
        isComposeWildcard(d) ||
        (d.compose_hashes.length > 0 && d.compose_hashes.every((h) => isHex(h, 32)))
      ) ||
      !(d.role === 'model' || d.role === 'gateway') ||
      !isHex(d.os_image_hash, 32) ||
      ![d.mr_td, d.rtmr0, d.rtmr1, d.rtmr2].every((v) => isHex(v, 48));
    if (bad) throw new PolicyError('the policy lists a deployment with a malformed value');
  }
  return policy;
}
