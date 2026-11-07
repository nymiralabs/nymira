// The agent recipes (IronClaw, Cursor) live in `shared/recipes.json`. This module turns one recipe
// into blocks, filling `{{name}}` placeholders. The README sections are written from it by
// `scripts/recipes.mjs`, and the web app's Agents screen draws the same blocks, so the two cannot
// drift (`web/test/recipes.test.ts` compares them). It names no loopback address: the web bundle
// check refuses those, so the callers supply them.

/** Replaces `{{key}}` with `vars[key]`. A key with no value is an error, never an empty string. */
export function fill(text, vars) {
  return text.replace(/\{\{(\w+)\}\}/g, (_, key) => {
    if (!Object.hasOwn(vars, key)) throw new Error(`recipe placeholder {{${key}}} has no value`);
    return vars[key];
  });
}

const MCP_JS = 'node_modules/@nymiralabs/nymira-mcp/dist/index.js';

/**
 * The MCP entry for Cursor: what `mcp/tools/host-config.ts --host cursor` prints. `m` holds raw
 * (unquoted) values: installDir, node, tokenFile, transport ('tcp' or 'unix'), endpoint (a URL
 * for tcp, the socket path for unix).
 */
export function mcpEntry(m) {
  const args = [
    `${m.installDir.replace(/\/$/, '')}/${MCP_JS}`,
    '--token-file',
    m.tokenFile,
    ...(m.transport === 'unix'
      ? ['--transport', 'unix', '--socket', m.endpoint]
      : ['--transport', 'tcp', '--url', m.endpoint]),
  ];
  return { mcpServers: { nymira: { type: 'stdio', command: m.node, args } } };
}

/**
 * One recipe as a list of blocks: `{t: 'p', text}`, `{t: 'ul', items}` or
 * `{t: 'code', lang, title, text}`. `vars` fills the placeholders in text and code; `vars.mcp`
 * (raw values, see `mcpEntry`) builds the Cursor entry.
 */
export function renderBlocks(recipe, vars) {
  return recipe.blocks.map((b) => {
    if (b.t === 'p') return { t: 'p', text: fill(b.text, vars) };
    if (b.t === 'ul') return { t: 'ul', items: b.items.map((i) => fill(i, vars)) };
    if (b.t === 'code') return { t: 'code', lang: b.lang, title: b.title, text: fill(b.text, vars) };
    if (b.t === 'mcp') {
      if (!vars.mcp) throw new Error('recipe has an MCP entry and no values for it');
      return { t: 'code', lang: 'json', title: b.title, text: JSON.stringify(mcpEntry(vars.mcp), null, 2) };
    }
    throw new Error(`unknown recipe block ${b.t}`);
  });
}

/** The README text of a list of blocks. */
export function markdown(blocks) {
  return blocks
    .map((b) => {
      if (b.t === 'p') return b.text;
      if (b.t === 'ul') return b.items.map((i) => `- ${i}`).join('\n');
      return `\`\`\`${b.lang}\n${b.text}\n\`\`\``;
    })
    .join('\n\n');
}

/** Prose for a screen that shows no Markdown: link targets stay, markers go. */
export function plainText(text) {
  return text
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '$1 ($2)')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/`([^`]+)`/g, '$1');
}
