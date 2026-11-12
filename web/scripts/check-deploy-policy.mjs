// Refuse a site that would replace the current main policy with an older one.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const [currentFile, bundledFile, site, liveUrl] = process.argv.slice(2);
const version = (path) => {
  const value = JSON.parse(readFileSync(path, 'utf8')).version;
  if (!Number.isSafeInteger(value) || value < 1)
    throw new Error(`invalid policy version in ${path}`);
  return value;
};
const current = version(currentFile);
const bundled = version(bundledFile);
const sideFile = join(site, 'attestation-policy.json');
const effective = existsSync(sideFile) ? Math.max(bundled, version(sideFile)) : bundled;
if (effective < current)
  throw new Error(`site policy ${effective} is older than main policy ${current}`);
if (liveUrl) {
  const response = await fetch(liveUrl, { signal: AbortSignal.timeout(15_000), redirect: 'error' });
  if (response.status !== 404) {
    // An error status says nothing about what the host serves: stay on the safe side.
    if (!response.ok) throw new Error(`cannot check live policy: HTTP ${response.status}`);
    // A body that is not a policy cannot be newer than ours, and no client takes it. It is the
    // very file a deployment has to repair, so it must not block the deployment.
    let live;
    try {
      live = (await response.json()).version;
    } catch {
      live = undefined;
    }
    if (!Number.isSafeInteger(live) || live < 1) {
      console.warn('the live policy file is not a readable policy; it will be replaced');
    } else if (effective < live) {
      throw new Error(`site policy ${effective} is older than live policy ${live}`);
    }
  }
}
console.log(`site policy ${effective}; main policy ${current}`);
