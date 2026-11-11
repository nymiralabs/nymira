// A small, safe markdown renderer for what a model answers. The answer is untrusted text: it is
// parsed into a tree and drawn as React elements, never as HTML. Raw HTML stays text, an image is
// never fetched (it would tell a third party that this answer was read), a link must be http or
// https without credentials in it, and every link opens with no referrer.
//
// The tree is drawn with a character budget, so an answer can be written out in order, with the
// rest of the text hidden in place and the layout fixed from the first frame.
import { Fragment, useState, type ReactNode } from 'react';

export type Inline =
  | { t: 'text'; s: string }
  | { t: 'code'; s: string }
  | { t: 'strong' | 'em' | 'del'; c: Inline[] }
  | { t: 'link'; href: string; c: Inline[] }
  | { t: 'br' }
  | { t: 'img'; alt: string };

export type Align = 'left' | 'center' | 'right' | null;

export type Block =
  | { t: 'p'; c: Inline[] }
  | { t: 'h'; level: 1 | 2 | 3 | 4; c: Inline[] }
  | { t: 'code'; lang: string; s: string }
  | { t: 'quote'; c: Block[] }
  | { t: 'list'; ordered: boolean; start: number; items: Block[][] }
  | { t: 'table'; head: Inline[][]; rows: Inline[][][]; align: Align[] }
  | { t: 'hr' };

/** Longest text that is parsed as markdown; the rest of a longer answer is kept as plain text. */
const MAX_PARSED = 100_000;
const MAX_DEPTH = 6;

// ---------------------------------------------------------------- inline

/** The link target if it is safe to follow: http(s), no user or password in it. */
export function safeHref(raw: string): string | null {
  try {
    const url = new URL(raw);
    if (url.protocol !== 'https:' && url.protocol !== 'http:') return null;
    if (url.username !== '' || url.password !== '') return null;
    return url.href;
  } catch {
    return null;
  }
}

interface Rule {
  re: RegExp;
  /** Builds the node for a match, or null when the match is not one after all. */
  make: (m: RegExpExecArray, src: string, depth: number) => Inline | null;
}

const word = /[A-Za-z0-9]/;

const RULES: Rule[] = [
  {
    re: /\\([\\`*_{}[\]()#+\-.!|~>])/g,
    make: (m) => ({ t: 'text', s: m[1]! }),
  },
  {
    re: /!\[([^\]\n]*)\]\([^)\n]*\)/g,
    make: (m) => ({ t: 'img', alt: m[1]! }),
  },
  {
    re: /`([^`\n]+)`/g,
    make: (m) => ({ t: 'code', s: m[1]! }),
  },
  {
    re: /\[([^\]\n]+)\]\(\s*(https?:\/\/[^\s)]+)(?:\s+"[^"\n]*")?\s*\)/g,
    make: (m, _src, depth) => {
      const href = safeHref(m[2]!);
      return href ? { t: 'link', href, c: parseInline(m[1]!, depth + 1) } : null;
    },
  },
  {
    re: /\*\*(?=\S)([\s\S]*?\S)\*\*|__(?=\S)([\s\S]*?\S)__(?![A-Za-z0-9_])/g,
    make: (m, src, depth) => {
      if (m[0]!.startsWith('__') && word.test(src[m.index - 1] ?? ' ')) return null;
      return { t: 'strong', c: parseInline(m[1] ?? m[2]!, depth + 1) };
    },
  },
  {
    re: /~~(?=\S)([\s\S]*?\S)~~/g,
    make: (m, _src, depth) => ({ t: 'del', c: parseInline(m[1]!, depth + 1) }),
  },
  {
    re: /\*(?=[^\s*])([\s\S]*?[^\s*])\*(?!\*)|_(?=[^\s_])([\s\S]*?[^\s_])_(?![A-Za-z0-9_])/g,
    make: (m, src, depth) => {
      if (m[0]!.startsWith('_') && word.test(src[m.index - 1] ?? ' ')) return null;
      return { t: 'em', c: parseInline(m[1] ?? m[2]!, depth + 1) };
    },
  },
  {
    re: /https?:\/\/[^\s<>()]+[^\s<>().,;:!?'")\]]/g,
    make: (m) => {
      const href = safeHref(m[0]!);
      return href ? { t: 'link', href, c: [{ t: 'text', s: m[0]! }] } : null;
    },
  },
  {
    re: /\n/g,
    make: () => ({ t: 'br' }),
  },
];

export function parseInline(src: string, depth = 0): Inline[] {
  if (depth > MAX_DEPTH) return src === '' ? [] : [{ t: 'text', s: src }];
  const out: Inline[] = [];
  let pos = 0;
  let text = '';
  const flush = () => {
    if (text !== '') out.push({ t: 'text', s: text });
    text = '';
  };
  // The next match of each rule at or after `pos`, found once and kept until it is passed.
  const next: (RegExpExecArray | null | undefined)[] = RULES.map(() => undefined);
  while (pos < src.length) {
    let best = -1;
    for (let i = 0; i < RULES.length; i++) {
      let m = next[i];
      if (m === undefined || (m !== null && m.index < pos)) {
        RULES[i]!.re.lastIndex = pos;
        m = RULES[i]!.re.exec(src);
        next[i] = m;
      }
      if (m && (best < 0 || m.index < next[best]!.index)) best = i;
    }
    if (best < 0) break;
    const m = next[best]!;
    const node = RULES[best]!.make(m, src, depth);
    if (node === null) {
      // Not a match after all: its first character is plain text and the search goes on.
      text += src.slice(pos, m.index + 1);
      pos = m.index + 1;
      next[best] = undefined;
      continue;
    }
    text += src.slice(pos, m.index);
    flush();
    out.push(node);
    pos = m.index + m[0].length;
  }
  text += src.slice(pos);
  flush();
  return out;
}

// ---------------------------------------------------------------- blocks

const FENCE = /^\s{0,3}(`{3,}|~{3,})\s*([\w+#.-]*)[^`]*$/;
const HR = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_RULE = /^\s*\|?\s*:?-{2,}:?\s*(?:\|\s*:?-{2,}:?\s*)*\|?\s*$/;

const indentOf = (line: string) => line.length - line.trimStart().length;

function cells(line: string): string[] {
  let s = line.trim();
  if (s.startsWith('|')) s = s.slice(1);
  if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
  const out: string[] = [];
  let cell = '';
  for (let k = 0; k < s.length; k++) {
    if (s[k] === '\\' && s[k + 1] === '|') {
      cell += '|';
      k++;
    } else if (s[k] === '|') {
      out.push(cell.trim());
      cell = '';
    } else cell += s[k];
  }
  out.push(cell.trim());
  return out;
}

function startsBlock(line: string, next: string | undefined): boolean {
  return (
    FENCE.test(line) ||
    HR.test(line) ||
    HEADING.test(line) ||
    /^\s{0,3}>/.test(line) ||
    ITEM.test(line) ||
    (line.includes('|') && next !== undefined && TABLE_RULE.test(next) && next.includes('-'))
  );
}

export function parseBlocks(src: string, depth = 0): Block[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (line.trim() === '') {
      i++;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const mark = fence[1]!;
      const body: string[] = [];
      i++;
      while (i < lines.length) {
        const close = /^\s{0,3}(`{3,}|~{3,})\s*$/.exec(lines[i]!);
        if (close && close[1]![0] === mark[0] && close[1]!.length >= mark.length) {
          i++;
          break;
        }
        body.push(lines[i]!);
        i++;
      }
      out.push({ t: 'code', lang: fence[2] ?? '', s: body.join('\n') });
      continue;
    }
    if (HR.test(line)) {
      out.push({ t: 'hr' });
      i++;
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      out.push({
        t: 'h',
        level: Math.min(4, heading[1]!.length) as 1 | 2 | 3 | 4,
        c: parseInline(heading[2]!),
      });
      i++;
      continue;
    }
    if (/^\s{0,3}>/.test(line) && depth < MAX_DEPTH) {
      const body: string[] = [];
      while (i < lines.length && /^\s{0,3}>/.test(lines[i]!)) {
        body.push(lines[i]!.replace(/^\s{0,3}>\s?/, ''));
        i++;
      }
      out.push({ t: 'quote', c: parseBlocks(body.join('\n'), depth + 1) });
      continue;
    }
    const after = lines[i + 1];
    if (
      line.includes('|') &&
      after !== undefined &&
      TABLE_RULE.test(after) &&
      after.includes('-')
    ) {
      const head = cells(line);
      const align = cells(after).map((c): Align => {
        const l = c.startsWith(':');
        const r = c.endsWith(':');
        return l && r ? 'center' : r ? 'right' : l ? 'left' : null;
      });
      const rows: Inline[][][] = [];
      i += 2;
      while (i < lines.length && lines[i]!.trim() !== '' && lines[i]!.includes('|')) {
        const row = cells(lines[i]!);
        rows.push(head.map((_, k) => parseInline(row[k] ?? '')));
        i++;
      }
      out.push({ t: 'table', head: head.map((h) => parseInline(h)), rows, align });
      continue;
    }
    const item = ITEM.exec(line);
    if (item && depth < MAX_DEPTH) {
      const base = indentOf(line);
      const ordered = /\d/.test(item[2]!);
      const start = ordered ? Number.parseInt(item[2]!, 10) : 1;
      const items: Block[][] = [];
      while (i < lines.length) {
        const m = ITEM.exec(lines[i]!);
        if (!m || indentOf(lines[i]!) !== base || /\d/.test(m[2]!) !== ordered) break;
        const body: string[] = [m[3]!];
        i++;
        // The item goes on while lines are indented deeper (or blank between such lines).
        while (i < lines.length) {
          const l = lines[i]!;
          if (l.trim() === '') {
            const peek = lines.slice(i + 1).find((x) => x.trim() !== '');
            if (peek !== undefined && indentOf(peek) > base) {
              body.push('');
              i++;
              continue;
            }
            break;
          }
          if (indentOf(l) <= base) break;
          body.push(l.slice(Math.min(indentOf(l), base + 2)));
          i++;
        }
        items.push(parseBlocks(body.join('\n'), depth + 1));
        while (i < lines.length && lines[i]!.trim() === '') {
          const peek = lines.slice(i + 1).find((x) => x.trim() !== '');
          const again = peek !== undefined ? ITEM.exec(peek) : null;
          if (again && indentOf(peek!) === base) i++;
          else break;
        }
      }
      out.push({ t: 'list', ordered, start, items });
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i]!.trim() !== '') {
      if (para.length > 0 && startsBlock(lines[i]!, lines[i + 1])) break;
      para.push(lines[i]!);
      i++;
    }
    out.push({ t: 'p', c: parseInline(para.join('\n').trim()) });
  }
  return out;
}

/** The markdown of an answer as a tree; text past the parse limit stays plain text. */
export function parseMarkdown(src: string): Block[] {
  if (src.length <= MAX_PARSED) return parseBlocks(src);
  return [
    ...parseBlocks(src.slice(0, MAX_PARSED)),
    { t: 'p', c: [{ t: 'text', s: src.slice(MAX_PARSED) }] },
  ];
}

// ---------------------------------------------------------------- length

const inlineLength = (c: Inline[]): number =>
  c.reduce((n, x) => {
    switch (x.t) {
      case 'text':
      case 'code':
        return n + x.s.length;
      case 'img':
        return n + imageText(x.alt).length;
      case 'br':
        return n;
      default:
        return n + inlineLength(x.c);
    }
  }, 0);

/** Characters of text a tree holds: what the written-out reveal counts. */
export function textLength(blocks: Block[]): number {
  return blocks.reduce((n, b) => {
    switch (b.t) {
      case 'p':
      case 'h':
        return n + inlineLength(b.c);
      case 'code':
        return n + b.s.length;
      case 'quote':
        return n + textLength(b.c);
      case 'list':
        return n + b.items.reduce((m, item) => m + textLength(item), 0);
      case 'table':
        return (
          n +
          b.head.reduce((m, c) => m + inlineLength(c), 0) +
          b.rows.reduce((m, r) => m + r.reduce((k, c) => k + inlineLength(c), 0), 0)
        );
      default:
        return n;
    }
  }, 0);
}

const imageText = (alt: string) => `[image${alt ? `: ${alt}` : ''}]`;

// ---------------------------------------------------------------- drawing

/** How much text is still to be shown, and whether the caret has been put down. */
export interface Reveal {
  left: number;
  caret: boolean;
}

function write(s: string, r: Reveal): ReactNode {
  if (s === '' || r.left >= s.length) {
    r.left = Math.max(0, r.left - s.length);
    return s;
  }
  const shown = s.slice(0, r.left);
  const rest = s.slice(r.left);
  r.left = 0;
  const caret = r.caret ? null : <i className="nc-caret" aria-hidden="true" />;
  r.caret = true;
  return (
    <>
      {shown}
      {caret}
      <span className="nc-typed-rest" aria-hidden="true">
        {rest}
      </span>
    </>
  );
}

function CopyCode({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="nc-md-copy"
      data-testid="md-copy"
      onClick={() => {
        navigator.clipboard?.writeText(text).then(
          () => {
            setDone(true);
            window.setTimeout(() => setDone(false), 1600);
          },
          () => undefined,
        );
      }}
    >
      {done ? 'Copied' : 'Copy'}
    </button>
  );
}

function inline(nodes: Inline[], r: Reveal): ReactNode {
  return nodes.map((n, i) => {
    switch (n.t) {
      case 'text':
        return <Fragment key={i}>{write(n.s, r)}</Fragment>;
      case 'code':
        return <code key={i}>{write(n.s, r)}</code>;
      case 'br':
        return <br key={i} />;
      case 'img':
        return (
          <span key={i} className="nc-md-img" title="Images in answers are not loaded">
            {write(imageText(n.alt), r)}
          </span>
        );
      case 'strong':
        return <strong key={i}>{inline(n.c, r)}</strong>;
      case 'em':
        return <em key={i}>{inline(n.c, r)}</em>;
      case 'del':
        return <del key={i}>{inline(n.c, r)}</del>;
      case 'link':
        return (
          <a
            key={i}
            href={n.href}
            target="_blank"
            rel="noopener noreferrer nofollow"
            referrerPolicy="no-referrer"
            title={n.href}
          >
            {inline(n.c, r)}
          </a>
        );
    }
  });
}

export function blocks(nodes: Block[], r: Reveal): ReactNode {
  return nodes.map((b, i) => {
    // A block the writing has not reached yet keeps its place and shows nothing, frame included.
    const pending = r.left === 0 && r.caret ? true : undefined;
    switch (b.t) {
      case 'p':
        return (
          <p key={i} data-pending={pending}>
            {inline(b.c, r)}
          </p>
        );
      case 'h': {
        const H = (['h3', 'h3', 'h4', 'h5'] as const)[b.level - 1]!;
        return (
          <H key={i} className="nc-md-h" data-level={b.level} data-pending={pending}>
            {inline(b.c, r)}
          </H>
        );
      }
      case 'hr':
        return <hr key={i} data-pending={pending} />;
      case 'quote':
        return (
          <blockquote key={i} data-pending={pending}>
            {blocks(b.c, r)}
          </blockquote>
        );
      case 'code':
        return (
          <figure key={i} className="nc-md-code" data-pending={pending}>
            <figcaption>
              <span>{b.lang || 'text'}</span>
              <CopyCode text={b.s} />
            </figcaption>
            <pre tabIndex={0}>
              <code>{write(b.s, r)}</code>
            </pre>
          </figure>
        );
      case 'list': {
        const Tag = b.ordered ? 'ol' : 'ul';
        return (
          <Tag
            key={i}
            data-pending={pending}
            {...(b.ordered && b.start !== 1 ? { start: b.start } : {})}
          >
            {b.items.map((item, k) => (
              <li key={k}>{blocks(item, r)}</li>
            ))}
          </Tag>
        );
      }
      case 'table':
        return (
          <div key={i} className="nc-md-table" tabIndex={0} data-pending={pending}>
            <table>
              <thead>
                <tr>
                  {b.head.map((c, k) => (
                    <th key={k} scope="col" data-align={b.align[k] ?? undefined}>
                      {inline(c, r)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row, k) => (
                  <tr key={k}>
                    {row.map((c, j) => (
                      <td key={j} data-align={b.align[j] ?? undefined}>
                        {inline(c, r)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
    }
  });
}

/** The tree with `shown` characters of its text visible and the rest in place, unseen. */
export function Markdown({ tree, shown }: { tree: Block[]; shown: number }) {
  return <>{blocks(tree, { left: shown, caret: false })}</>;
}
