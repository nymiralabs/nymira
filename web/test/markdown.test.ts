import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import {
  Markdown,
  parseBlocks,
  parseInline,
  parseMarkdown,
  safeHref,
  textLength,
} from '../src/ui/markdown';

const html = (src: string, shown = Infinity) =>
  renderToStaticMarkup(createElement(Markdown, { tree: parseMarkdown(src), shown }));

describe('markdown blocks', () => {
  it('reads headings, paragraphs, rules and quotes', () => {
    const b = parseBlocks('# Title\n\nOne\ntwo\n\n---\n\n> quoted\n> more');
    expect(b.map((x) => x.t)).toEqual(['h', 'p', 'hr', 'quote']);
  });

  it('keeps a fenced block as it is, with its language', () => {
    const b = parseBlocks('```rust\nfn main() {\n  **not bold**\n}\n```\nafter');
    expect(b[0]).toEqual({ t: 'code', lang: 'rust', s: 'fn main() {\n  **not bold**\n}' });
    expect(b[1]!.t).toBe('p');
  });

  it('closes an unfinished fence at the end', () => {
    expect(parseBlocks('```\nopen')[0]).toEqual({ t: 'code', lang: '', s: 'open' });
  });

  it('nests lists by indent and keeps ordered numbering', () => {
    const b = parseBlocks('3. a\n4. b\n   - x\n   - y\n5. c');
    const list = b[0]!;
    expect(list.t).toBe('list');
    if (list.t !== 'list') return;
    expect(list.ordered).toBe(true);
    expect(list.start).toBe(3);
    expect(list.items).toHaveLength(3);
    expect(list.items[1]!.map((x) => x.t)).toEqual(['p', 'list']);
  });

  it('reads a table with alignment', () => {
    const b = parseBlocks('| a | b |\n|:--|--:|\n| 1 | 2 |\n| 3 |');
    const t = b[0]!;
    expect(t.t).toBe('table');
    if (t.t !== 'table') return;
    expect(t.align).toEqual(['left', 'right']);
    expect(t.rows).toHaveLength(2);
    expect(t.rows[1]).toHaveLength(2);
  });

  it('does not take a lone pipe line for a table', () => {
    expect(parseBlocks('a | b\nplain')[0]!.t).toBe('p');
  });
});

describe('markdown inline', () => {
  it('reads emphasis, code and strike', () => {
    expect(parseInline('**b** *i* `c` ~~d~~').map((x) => x.t)).toEqual([
      'strong',
      'text',
      'em',
      'text',
      'code',
      'text',
      'del',
    ]);
  });

  it('leaves snake_case and a lone star alone', () => {
    expect(parseInline('use my_var_name and 2 * 3 * 4')).toEqual([
      { t: 'text', s: 'use my_var_name and 2 * 3 * 4' },
    ]);
  });

  it('does not close an underscore mark inside a word', () => {
    expect(parseInline('window.__pwned = 1 and other.__thing = 2').map((x) => x.t)).toEqual([
      'text',
    ]);
  });

  it('lets a backslash escape a mark', () => {
    expect(parseInline('\\*not italic\\*')).toEqual([
      { t: 'text', s: '*' },
      { t: 'text', s: 'not italic' },
      { t: 'text', s: '*' },
    ]);
  });

  it('makes a single newline a line break', () => {
    expect(parseInline('a\nb').map((x) => x.t)).toEqual(['text', 'br', 'text']);
  });
});

describe('markdown is safe', () => {
  it('shows raw HTML as text', () => {
    const out = html('<script>alert(1)</script> <img src=x onerror=alert(1)> <b>x</b>');
    expect(out).not.toContain('<script');
    expect(out).not.toContain('<img');
    expect(out).toContain('&lt;script&gt;');
  });

  it('never loads an image', () => {
    const out = html('![tracker](https://evil.example/p.png?u=1)');
    expect(out).not.toContain('<img');
    expect(out).not.toContain('evil.example');
    expect(out).toContain('[image: tracker]');
  });

  it('links only http(s) targets without credentials', () => {
    expect(safeHref('javascript:alert(1)')).toBeNull();
    expect(safeHref('data:text/html,x')).toBeNull();
    expect(safeHref('https://good.example@evil.example/')).toBeNull();
    expect(safeHref('https://user:pw@example.org/')).toBeNull();
    expect(safeHref('https://example.org/a?b=1')).toBe('https://example.org/a?b=1');
    const out = html('[click](javascript:alert(1)) and [ok](https://example.org)');
    expect(out).not.toContain('href="javascript');
    expect(out).toContain('href="https://example.org/"');
  });

  it('opens a link with no referrer and no opener', () => {
    const out = html('[ok](https://example.org)');
    expect(out).toContain('rel="noopener noreferrer nofollow"');
    expect(out).toContain(
      'referrerPolicy="no-referrer"'.replace('referrerPolicy', 'referrerPolicy'),
    );
    expect(out).toContain('target="_blank"');
  });

  it('survives deep nesting and long runs of marks', () => {
    const deep = '> '.repeat(40) + 'x';
    expect(() => parseMarkdown(deep)).not.toThrow();
    const stars = '*'.repeat(20_000) + 'a' + '*'.repeat(20_000);
    expect(() => parseMarkdown(stars)).not.toThrow();
    const brackets = '[a](https://x.example '.repeat(2000);
    const started = Date.now();
    parseMarkdown(brackets);
    expect(Date.now() - started).toBeLessThan(2000);
  });
});

describe('markdown reveal', () => {
  const src = '# Hi\n\nSome **bold** text.\n\n```js\nlet a = 1;\n```';

  it('counts the text a tree holds', () => {
    expect(textLength(parseMarkdown(src))).toBe(
      'Hi'.length + 'Some bold text.'.length + 'let a = 1;'.length,
    );
  });

  it('shows all of it when fully revealed, with no caret', () => {
    const out = html(src);
    expect(out).not.toContain('nc-caret');
    expect(out).not.toContain('nc-typed-rest');
    expect(out).toContain('<strong>bold</strong>');
  });

  it('keeps the whole text in place while part of it is hidden', () => {
    const out = html(src, 7);
    expect(out).toContain('nc-caret');
    expect(out).toContain('nc-typed-rest');
    // The text is all there for layout: shown part plus hidden rest equals the full text.
    const text = out.replace(/<[^>]*>/g, '');
    expect(text).toContain('Some bold text.');
    expect(text).toContain('let a = 1;');
  });
});
