// Fails when a vendored NEAR Connect executor and its pin disagree. `--write-fixture` rewrites the
// hash of the test-only executor (the real ones are updated by hand, on purpose, with a diff).
import { createHash } from 'node:crypto';
import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const sets = [
  { pins: 'pins/executors.json', dir: 'public/executors', fixture: false },
  { pins: 'test-fixtures/executors.json', dir: 'test-fixtures/executors', fixture: true },
];
const write = process.argv.includes('--write-fixture');
let failed = false;

for (const set of sets) {
  const path = join(root, set.pins);
  const table = JSON.parse(readFileSync(path, 'utf8'));
  for (const [id, pin] of Object.entries(table.wallets)) {
    const file = join(root, set.dir, pin.file);
    const bytes = readFileSync(file);
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    if (set.fixture && write) {
      pin.sha256 = sha256;
      pin.bytes = statSync(file).size;
      continue;
    }
    if (sha256 !== pin.sha256 || bytes.length !== pin.bytes) {
      failed = true;
      console.error(
        `${id}: ${pin.file} has sha256 ${sha256} (${bytes.length} bytes), pinned ${pin.sha256} (${pin.bytes})`,
      );
    } else {
      console.log(`${id}: ok ${sha256.slice(0, 16)}`);
    }
  }
  if (set.fixture && write) writeFileSync(path, JSON.stringify(table, null, 2) + '\n');
}
process.exit(failed ? 1 : 0);
