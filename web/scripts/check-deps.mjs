// node scripts/check-deps.mjs <package-lock.json> <exceptions.json>
// Fails on a package younger than the minimum release age, a lockfile hash that is not the
// registry's, or a version with no provenance attestation that the exceptions do not name.
// Exceptions: `{ "provenance": { "name@version": "why" }, "age": { ... } }` (lib/deps.mjs).
// `--write` prints the entries the lockfile needs.
import { readFileSync } from 'node:fs';
import { checkPackages, lockedPackages } from './lib/deps.mjs';

const [lockFile, exceptionsFile, flag] = process.argv.slice(2);
if (!lockFile || !exceptionsFile) {
  console.error('usage: check-deps.mjs <package-lock.json> <exceptions.json> [--write]');
  process.exit(2);
}
const lock = JSON.parse(readFileSync(lockFile, 'utf8'));
const exceptions = JSON.parse(readFileSync(exceptionsFile, 'utf8'));
const cache = new Map();
const packument = async (name) => {
  if (!cache.has(name)) {
    const res = await fetch(`https://registry.npmjs.org/${name.replace('/', '%2f')}`);
    if (!res.ok) throw new Error(`registry: ${name}: HTTP ${res.status}`);
    cache.set(name, await res.json());
  }
  return cache.get(name);
};
const { problems, noProvenance, tooYoung, stale } = await checkPackages(
  lockedPackages(lock),
  packument,
  exceptions,
);
if (flag === '--write') {
  const entries = (ids, why) => Object.fromEntries(ids.map((id) => [id, why]));
  console.log(
    JSON.stringify(
      {
        provenance: {
          ...exceptions.provenance,
          ...entries(noProvenance, 'no attestation published'),
        },
        age: entries(
          tooYoung,
          'locked before the minimum release age rule; goes stale after 7 days',
        ),
      },
      null,
      2,
    ),
  );
  process.exit(0);
}
for (const id of stale) console.warn(`warning: ${id}: age exception no longer needed; remove it`);
for (const id of noProvenance)
  problems.push(`${id}: no provenance attestation; add it to ${exceptionsFile} with a reason`);
if (problems.length) {
  console.error(problems.join('\n'));
  process.exit(1);
}
console.log(
  `${lockFile}: every package is old enough, matches the registry, and has provenance or a listed reason`,
);
