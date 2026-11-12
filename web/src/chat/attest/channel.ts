// A newer signed policy beside the bundle (ADR 0021). The site serves `/attestation-policy.json`
// next to the app. It changes without a new web release when NEAR AI redeploys a model. The page
// takes it only when it verifies under the signer key built into the bundle and its version is
// higher than the bundled one; anything else (no file, bad signature, older version, a slow host)
// leaves the bundled policy in force. The file comes from the same origin as the app, so the fetch
// shows the host nothing a page load does not.
import { loadPolicy, type Policy } from './policy';

export const SIDE_FILE = '/attestation-policy.json';
const MAX_BYTES = 64 * 1024;

export interface PolicySource {
  policy: Policy;
  from: 'bundle' | 'site';
  /** Why a site file was not used. Absent when none was offered or it was used. */
  note?: string;
}

/** Check for updates on each conversation, retaining the newest policy seen in this page. */
export function policyUpdates(bundled: Policy, signerPk: string, get: typeof fetch = fetch) {
  let current: PolicySource = { policy: bundled, from: 'bundle' };
  return async (): Promise<PolicySource> => {
    const next = await newerPolicy(current.policy, signerPk, get);
    if (next.policy.version > current.policy.version) current = next;
    return current;
  };
}

export async function newerPolicy(
  bundled: Policy,
  signerPk: string,
  get: typeof fetch = fetch,
  timeoutMs = 5000,
): Promise<PolicySource> {
  const keep = (note?: string): PolicySource => ({ policy: bundled, from: 'bundle', note });
  try {
    const res = await get(SIDE_FILE, {
      cache: 'no-store',
      credentials: 'omit',
      redirect: 'error',
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (res.status === 404) return keep();
    if (!res.ok) return keep(`the site answered ${res.status}`);
    const text = await res.text();
    if (text.length > MAX_BYTES) return keep('the policy file is too large');
    const policy = loadPolicy(JSON.parse(text), signerPk, bundled.version + 1);
    return { policy, from: 'site' };
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    // A file that is not a newer, signed policy is not an error worth stopping a chat for.
    return keep(why.startsWith('the policy is version') ? undefined : why);
  }
}
