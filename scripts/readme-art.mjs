#!/usr/bin/env node
// Writes the figures of the README and the other public documents to `assets/readme/`, in a dark
// and a light edition of each (`<name>-dark.svg`, `<name>-light.svg`). The drawings use the
// console's own parts: cells, thin lines, one hatched accent, IBM Plex with system fallbacks.
// The wordmark paths and the write-on strokes come from web/src/marketing/Wordmark.tsx, so the
// README wordmark is the site's. Run it from the repository root: `node scripts/readme-art.mjs`.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'assets', 'readme');
mkdirSync(OUT, { recursive: true });

const PALETTE = {
  dark: { fg: '#fafafa', muted: '#a1a1a1', line: '#3a3a3a', cell: '#ffffff48', accent: '#9aa8ff', good: '#9dc9a4' },
  light: { fg: '#161616', muted: '#626262', line: '#d4d4d4', cell: '#00000040', accent: '#3b4cc0', good: '#2f6b3b' },
};

// ---- the wordmark, read from the site's source ----
const wm = readFileSync(join(ROOT, 'web/src/marketing/Wordmark.tsx'), 'utf8');
const LETTERS = /const LETTERS =\s*'([^']+)'/.exec(wm)?.[1];
const ACCENT = /const ACCENT =\s*'([^']+)'/.exec(wm)?.[1];
if (!LETTERS || !ACCENT) throw new Error('Wordmark.tsx: paths not found');
const STROKES = [
  { d: ['M113 490V165L415 480V160'], delay: 0 },
  { d: ['M525 255L650 450', 'M770 255L580 575'], delay: 0.3 },
  { d: ['M843 230V510', 'M843 330A87 87 0 0 1 1018 330V510', 'M1018 330A87 87 0 0 1 1193 330V510'], delay: 0.55 },
  { d: ['M1330 235V510'], delay: 0.85 },
  { d: ['M1445 510V235', 'M1445 380C1445 300 1490 250 1570 250'], delay: 1.0 },
  { d: ['M1771 245A117 115 0 1 0 1771 475A117 115 0 1 0 1771 245Z', 'M1888 230V510'], delay: 1.2 },
];
const FLIP = 'translate(0 715) scale(.1 -.1)';

const FONT = `font-family:'IBM Plex Sans',-apple-system,'Segoe UI',system-ui,sans-serif`;
const MONO = `font-family:'IBM Plex Mono',ui-monospace,SFMono-Regular,Menlo,Consolas,monospace`;

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

function frame({ w, h, title, desc, css = '', body, p }) {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" role="img" aria-labelledby="t d">
<title id="t">${esc(title)}</title>
<desc id="d">${esc(desc)}</desc>
<style>
text{${FONT};fill:${p.fg}}
.m{${MONO}}
.mu{fill:${p.muted}}
.ln{stroke:${p.line};fill:none;stroke-width:1}
.ac{stroke:${p.accent};fill:none;stroke-width:1}
.hatch{fill:url(#hatch)}
${css}
@media (prefers-reduced-motion:reduce){*{animation:none!important}}
</style>
<defs>
<pattern id="hatch" width="5" height="5" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="5" stroke="${p.accent}" stroke-width="1.2"/></pattern>
</defs>
${body}
</svg>
`;
}

/** A row of cells; `kinds[i]` is 's' solid, 'h' hatched, 'o' outlined, 'd' dashed, 'e' empty. */
function cells(x, y, kinds, { w = 16, h = 22, gap = 4, p, cls = 'cell', delay = 14 }) {
  return kinds
    .map((k, i) => {
      const cx = x + i * (w + gap);
      const style = `animation-delay:${i * delay}ms`;
      if (k === 's') return `<rect class="${cls}" x="${cx}" y="${y}" width="${w}" height="${h}" fill="${p.cell}" style="${style}"/>`;
      if (k === 'h') return `<g class="${cls}" style="${style}"><rect x="${cx}" y="${y}" width="${w}" height="${h}" class="hatch"/><rect x="${cx + 0.5}" y="${y + 0.5}" width="${w - 1}" height="${h - 1}" class="ac"/></g>`;
      if (k === 'o') return `<rect class="${cls} ac" x="${cx + 0.5}" y="${y + 0.5}" width="${w - 1}" height="${h - 1}" style="${style}"/>`;
      if (k === 'd') return `<rect class="${cls}" x="${cx + 0.5}" y="${y + 0.5}" width="${w - 1}" height="${h - 1}" fill="none" stroke="${p.muted}" stroke-dasharray="3 2" style="${style}"/>`;
      return `<rect class="${cls} ln" x="${cx + 0.5}" y="${y + 0.5}" width="${w - 1}" height="${h - 1}" style="${style}"/>`;
    })
    .join('\n');
}

const CELL_CSS = `.cell{animation:cell-in .5s cubic-bezier(.16,1,.3,1) both}
@keyframes cell-in{from{opacity:0;transform:translateY(6px)}}`;

// ---------------------------------------------------------------- wordmark (animated)
const WM_CSS = `
.w mask path{stroke-dasharray:1;animation:write .35s cubic-bezier(.5,0,.3,1) both}
.hop,.drop{transform-box:fill-box;transform-origin:50% 100%}
.drop{animation:drop .75s cubic-bezier(.3,0,.5,1) 1.6s both}
.hop{animation:hop 6s ease-out 3.5s infinite}
@keyframes write{from{stroke-dashoffset:1}to{stroke-dashoffset:0}}
@keyframes drop{0%{transform:translateY(-700px);opacity:0}8%{opacity:1}50%{transform:translateY(0) scale(1.25,.6)}68%{transform:translateY(-130px) scale(.9,1.1)}82%{transform:translateY(0) scale(1.1,.8)}100%{transform:translateY(0) scale(1)}}
@keyframes hop{0%,8%,100%{transform:translateY(0) scale(1)}3%{transform:translateY(-120px) scale(.92,1.1)}5.5%{transform:translateY(0) scale(1.12,.82)}}`;

function wordmarkBody(p) {
  const strokes = STROKES.flatMap((l) =>
    l.d.map((d, j) => `<path d="${d}" pathLength="1" style="animation-delay:${(l.delay + j * 0.08).toFixed(2)}s"/>`),
  ).join('\n');
  return `<mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="2200" height="715"><g transform="scale(1.1)" fill="none" stroke="#fff" stroke-width="130" stroke-linecap="round" stroke-linejoin="round" class="w">
${strokes}
</g></mask>
<g fill="${p.fg}" class="w"><g mask="url(#m)"><path d="${LETTERS}" transform="${FLIP}"/></g>
<g class="hop"><g class="drop"><path d="${ACCENT}" transform="${FLIP}"/></g></g></g>`;
}

function wordmark(p) {
  return frame({ w: 2200, h: 715, title: 'Nymira', desc: 'The Nymira wordmark, written on, with the dot of the i dropping in and hopping now and then.', css: WM_CSS, body: wordmarkBody(p), p });
}

// ---------------------------------------------------------------- hero
function hero(p) {
  const strip = cells(0, 0, [...Array(11).fill('s'), 'h'], { w: 30, h: 36, gap: 6, p });
  const body = `
<g transform="translate(64 64) scale(.17)">${wordmarkBody(p)}</g>
<text x="66" y="236" font-size="30">Private, prepaid API credits on NEAR.</text>
<text x="66" y="276" class="m mu" font-size="17">testnet \u00b7 test funds only \u00b7 no independent review yet</text>
<g transform="translate(66 316)">${strip}</g>
<text x="66" y="394" class="m mu" font-size="15">vault.nymira.testnet \u00b7 nymira.pages.dev</text>`;
  return frame({
    w: 1000,
    h: 430,
    title: 'Nymira: private, prepaid API credits on NEAR',
    desc: 'The Nymira wordmark with the line "Private, prepaid API credits on NEAR", a note drawn as twelve cells with one empty cell, and the testnet notice.',
    css: CELL_CSS + WM_CSS,
    body,
    p,
  });
}

// ---------------------------------------------------------------- the note's days
function noteLife(p) {
  const w = 17;
  const gap = 4;
  const x0 = 48;
  const kinds = Array.from({ length: 37 }, (_, d) => (d >= 30 ? 'd' : d >= 25 ? 'h' : 's'));
  const right = (day) => x0 + day * (w + gap) + w;
  const mark = (x, anchor, top, bottom) =>
    `<text x="${x}" y="126" text-anchor="${anchor}" class="m mu" font-size="13">${top}</text><text x="${x}" y="146" text-anchor="${anchor}" font-size="15">${bottom}</text>`;
  const legend = [
    ['s', 'Open: spend it, or close it'],
    ['h', 'From day 25 (5 days left): no new lease, a close is tried. From day 27 (3 days left): an exit if the close failed.'],
    ['d', '7 days after expiry: the treasury can take it, unless an exit is pending.'],
  ]
    .map(([k, t], i) => `${cells(x0, 184 + i * 34, [k], { w: 22, h: 14, gap: 0, p, delay: 0 })}<text x="${x0 + 36}" y="${196 + i * 34}" font-size="15">${esc(t)}</text>`)
    .join('\n');
  const body = `
<text x="${x0}" y="44" font-size="22">The life of a note</text>
<text x="${x0}" y="68" class="mu" font-size="15">One cell is one day. The numbers come from the vault (30 days, a 7-day window) and the expiry guard (5 and 3 days).</text>
${cells(x0, 84, kinds, { w, h: 20, gap, p, delay: 16 })}
${mark(x0, 'start', 'day 0', 'Deposit')}
${mark(right(29), 'end', 'day 30', 'Expires')}
${mark(right(36), 'end', 'day 37', 'Treasury can take it')}
${legend}`;
  return frame({ w: 1000, h: 300, title: 'The life of a note', desc: 'A note is open for 30 days. At 5 days left the app tries a close, at 3 days left an exit if the close failed. Seven days after expiry the treasury can take the deposit unless an exit is pending.', css: CELL_CSS, body, p });
}

// ---------------------------------------------------------------- who does what
function parties(p) {
  const box = (x, y, w, h, title, lines) => `<rect x="${x}" y="${y}" width="${w}" height="${h}" class="ln"/>
<text x="${x + 20}" y="${y + 34}" font-size="19">${esc(title)}</text>
${lines.map((t, i) => `<text x="${x + 20}" y="${y + 62 + i * 22}" class="m mu" font-size="13">${esc(t)}</text>`).join('\n')}`;
  const arrow = (d, label, lx, ly, anchor = 'middle') => `<path d="${d}" class="ln flow" stroke-dasharray="5 5" marker-end="url(#ah)"/>
${label.split('|').map((t, i) => `<text x="${lx}" y="${ly + i * 17}" text-anchor="${anchor}" font-size="13.5">${esc(t)}</text>`).join('\n')}`;
  const css = `.flow{animation:flow 1.4s linear infinite}@keyframes flow{to{stroke-dashoffset:-20}}`;
  const body = `
<defs><marker id="ah" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto"><path d="M1 1L9 5L1 9" fill="none" stroke="${p.muted}"/></marker></defs>
${box(30, 40, 250, 190, 'You', ['note secret', 'prompts and answers', 'proofs are made here', 'web app or nymirad'])}
${box(415, 40, 250, 190, 'Operator', ['sees: lease proof,', 'nullifier, bound class,', 'model, sizes, timing,', 'usage, charge', 'not your prompt text'])}
${box(800, 40, 250, 190, 'NEAR AI (TEE)', ['sees: your prompt (as', 'the model\u2019s enclave),', 'token counts, timing', 'not which note'])}
${box(415, 310, 250, 150, 'Vault (NEAR)', ['holds the USDC', 'deposit, then on a close:', 'note ID, total spend,', 'payout account'])}
${arrow('M280 130H415', 'lease proof,|encrypted prompt', 347, 90)}
${arrow('M665 130H800', 'encrypted|prompt', 732, 90)}
${arrow('M155 230V385H415', 'deposit, close, exit (wallet)', 175, 372, 'start')}
${arrow('M540 230V310', 'sync_root,|heartbeat', 552, 262, 'start')}
<text x="30" y="496" class="m mu" font-size="13">The web app cannot use Tor, so the operator and the RPC provider also see your IP address.</text>`;
  return frame({ w: 1080, h: 520, title: 'Who does what, and who sees what', desc: 'You send a lease proof and an encrypted prompt to the operator, which forwards the encrypted prompt to NEAR AI. You deposit, close and exit through the vault on NEAR. The operator syncs the vault root and sends heartbeats.', css, body, p });
}

// ---------------------------------------------------------------- the money
function money(p) {
  const row = (y, label, kinds, note) => `<text x="48" y="${y + 17}" font-size="16">${esc(label)}</text>
${cells(250, y, kinds, { w: 15, h: 22, gap: 3, p, delay: 10 })}
<text x="${250 + kinds.length * 18 + 18}" y="${y + 17}" class="m mu" font-size="13">${esc(note)}</text>`;
  const note = Array(20).fill('s');
  const body = `
<text x="48" y="44" font-size="22">Where the USDC goes</text>
<text x="48" y="68" class="mu" font-size="15">One cell is 0.05 USDC. An example: a short chat with z-ai/glm-5.3-flash on a 1 USDC note.</text>
${row(100, 'Deposit', [...note, 'h'], '1.05 = 1.00 note + 0.05 fee')}
${row(150, 'A lease opens', [...note.slice(0, 18), 'o', 'o'], '0.10 reserved, nothing spent yet')}
${row(200, 'The lease ends', [...note.slice(0, 19), 's'], 'charge 0.002, the rest is free again')}
${row(250, 'Close from the wallet', [...note.slice(0, 19), 's'], '0.998 to the payout account, no fee')}
<text x="48" y="318" class="mu" font-size="14">Charge = usage x 1.03 + a slot fee, rounded up to 0.001, at least 0.002, never above the bound.</text>`;
  return frame({ w: 1000, h: 340, title: 'Where the USDC goes', desc: 'A 1 USDC note costs 1.05 USDC with the fee. A lease reserves 0.10 USDC and, when it ends, charges its usage with a minimum of 0.002 USDC. A close from the wallet pays the rest to the payout account with no close fee.', css: CELL_CSS, body, p });
}

// ---------------------------------------------------------------- budget notes
function budget(p) {
  const layer = (y, h, title, lines, tone) => `<rect x="40" y="${y}" width="920" height="${h}" class="${tone}"/>
<text x="64" y="${y + 36}" font-size="19">${esc(title)}</text>
${lines.map((t, i) => `<text x="64" y="${y + 66 + i * 24}" class="m mu" font-size="14">${esc(t)}</text>`).join('\n')}`;
  const body = `
<text x="40" y="40" font-size="22">A budget note for one agent</text>
${layer(64, 134, 'The vault enforces the ceiling', ['An agent can spend at most the balance of its note.', 'A close can only pay your payout account, fixed at deposit.', 'This rests on the vault matching the client’s pins.'], 'ac')}
${layer(222, 168, 'nymirad on your machine enforces the policy', ['model list, lease, call and daily limits, rate limit', 'your approval for a call above confirm_above_usd', 'require_tee: an answer is released after its signature is checked', 'If the note moves to the agent’s machine, the policy is advisory.'], 'ln')}
<text x="40" y="426" class="m mu" font-size="13">The agent token starts with nyma_ and reaches chat, models, balance and quote only. No remote kill switch exists.</text>`;
  return frame({ w: 1000, h: 446, title: 'A budget note for one agent', desc: 'The vault enforces the ceiling: the note balance, and a close that pays only your payout account. Your own nymirad enforces the policy: models, limits, rate, approvals and signature checks.', css: '', body, p });
}

const FIGURES = { wordmark, hero, 'note-life': noteLife, parties, money, budget };
for (const [name, make] of Object.entries(FIGURES)) {
  for (const [theme, p] of Object.entries(PALETTE)) {
    writeFileSync(join(OUT, `${name}-${theme}.svg`), make(p));
  }
}
console.log(`wrote ${Object.keys(FIGURES).length * 2} files to assets/readme/`);
