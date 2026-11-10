#!/usr/bin/env node
// Writes the IronClaw and Cursor recipe sections of BUDGET_NOTES.md from shared/recipes.json, or checks
// that the README matches it:
//
//   node scripts/recipes.mjs --write
//   node scripts/recipes.mjs --check
//
// The README shows each recipe with the example values below. The Agents screen fills the same
// recipes from what the owner types (web/src/agents/recipes.ts).

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { markdown, renderBlocks } from '../shared/recipes.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
export const README = join(ROOT, 'BUDGET_NOTES.md');
export const SOURCE = JSON.parse(readFileSync(join(ROOT, 'shared', 'recipes.json'), 'utf8'));

/** The example values the README shows. */
export const DEFAULTS = {
  ironclaw: {
    name: 'ironclaw',
    token_file: '/srv/agent/ironclaw.token',
    endpoint: 'http://localhost:8788',
    local: 'localhost',
    agent_host: 'AGENT_HOST',
    agent_user: 'AGENT_USER',
  },
  cursor: {
    name: 'cursor',
    local: 'localhost',
    mcp: {
      installDir: '/opt/nymira-mcp',
      node: '/usr/local/bin/node',
      tokenFile: '/srv/agent/cursor.token',
      transport: 'tcp',
      endpoint: 'http://127.0.0.1:8788',
    },
  },
};

const begin = (id) => `<!-- recipe:${id} begin: written from shared/recipes.json by scripts/recipes.mjs, do not edit here -->`;
const end = (id) => `<!-- recipe:${id} end -->`;

/** The README text of one recipe, between its markers. */
export function section(id) {
  const recipe = SOURCE.recipes.find((r) => r.id === id);
  return `${begin(id)}\n\n${markdown(renderBlocks(recipe, DEFAULTS[id]))}\n\n${end(id)}`;
}

/** The README with every recipe section replaced by a fresh one. */
export function rewrite(readme) {
  let out = readme;
  for (const r of SOURCE.recipes) {
    const re = new RegExp(`<!-- recipe:${r.id} begin[^\\n]*-->[\\s\\S]*?<!-- recipe:${r.id} end -->`);
    if (!re.test(out)) throw new Error(`README has no markers for recipe ${r.id}`);
    out = out.replace(re, () => section(r.id));
  }
  return out;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const current = readFileSync(README, 'utf8');
  const next = rewrite(current);
  if (process.argv.includes('--write')) {
    writeFileSync(README, next);
  } else if (process.argv.includes('--check')) {
    if (next !== current) {
      console.error('BUDGET_NOTES.md differs from shared/recipes.json: run node scripts/recipes.mjs --write');
      process.exit(1);
    }
  } else {
    console.error('usage: node scripts/recipes.mjs --write | --check');
    process.exit(2);
  }
}
