// Checks every package of an npm lockfile against the registry's own record of it:
//  - the lockfile's integrity hash is the registry's (a hand-edited lockfile fails);
//  - the version was published at least `minAgeDays` ago (a fresh malicious release gets time to
//    be found and pulled before it can enter a build);
//  - the version carries a provenance attestation, or is listed with a reason in the exceptions
//    file. `npm audit signatures` (a CI step) then verifies the signatures and attestations.
// The exceptions file is `{ "provenance": { "name@version": "why" }, "age": { ... } }`. An age
// exception is for a version already in the lockfile; it is flagged for removal once the version
// is old enough; the check warns about it and does not fail.
export const MIN_AGE_DAYS = 7;

/** `[{ name, version, integrity }]` for every registry package in a lockfile. */
export function lockedPackages(lock) {
  const out = [];
  for (const [path, entry] of Object.entries(lock.packages ?? {})) {
    if (!path || entry.link) continue;
    const name = path.slice(path.lastIndexOf('node_modules/') + 'node_modules/'.length);
    if (!entry.resolved?.startsWith('https://registry.npmjs.org/')) {
      out.push({
        name,
        version: entry.version,
        integrity: entry.integrity,
        problem: 'not from the npm registry',
      });
    } else out.push({ name, version: entry.version, integrity: entry.integrity });
  }
  return out;
}

/**
 * `packument(name)` returns the registry document of a package. Returns `{ problems, noProvenance }`:
 * `noProvenance` lists `name@version` entries without an attestation that the exceptions do not cover.
 */
export async function checkPackages(
  packages,
  packument,
  exceptions,
  now = Date.now(),
  minAgeDays = MIN_AGE_DAYS,
) {
  const noProv = exceptions.provenance ?? {};
  const young = exceptions.age ?? {};
  const stale = new Set(Object.keys(young));
  const problems = [];
  const missing = new Set();
  const tooYoung = new Set();
  const seen = new Set();
  for (const p of packages) {
    const id = `${p.name}@${p.version}`;
    if (seen.has(id)) continue;
    seen.add(id);
    if (p.problem) {
      problems.push(`${id}: ${p.problem}`);
      continue;
    }
    const doc = await packument(p.name);
    const v = doc.versions?.[p.version];
    const published = Date.parse(doc.time?.[p.version] ?? '');
    if (!v || Number.isNaN(published)) {
      problems.push(`${id}: not in the registry`);
      continue;
    }
    if (v.dist?.integrity !== p.integrity)
      problems.push(`${id}: lockfile integrity differs from the registry's`);
    const days = (now - published) / 86_400_000;
    if (days < minAgeDays) {
      if (id in young) stale.delete(id);
      else tooYoung.add(id);
    }
    if (!v.dist?.attestations?.provenance && !(id in noProv)) missing.add(id);
  }
  for (const id of Object.keys(noProv)) {
    if (!seen.has(id))
      problems.push(`${id}: provenance exception but not in the lockfile; remove it`);
  }
  for (const id of tooYoung) problems.push(`${id}: younger than ${minAgeDays} days`);
  return {
    problems,
    noProvenance: [...missing].sort(),
    tooYoung: [...tooYoung].sort(),
    // A stale age exception is a warning: it turns stale with time, not with a change.
    stale: [...stale].sort(),
  };
}
