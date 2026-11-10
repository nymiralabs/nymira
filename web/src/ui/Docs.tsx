import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Wordmark } from '../marketing/Wordmark';
import { ThemeToggle } from '../marketing/ThemeToggle';
import { vars } from '../marketing/kit';
import { PROFILE } from '../profile';
import { ExitWindow, Lease, NoteLife, Parties } from './docs-diagrams';
import { ChargeCalc } from './docs-calc';
import { IpMatrix, LivePanel, NoteCalendar, RawView, VaultCompare } from './docs-extras';
import { PAGE, type Block, type DocLink, type Section } from './docs-content';
import './docs.css';
import './docs-figures.css';

// The glossary: terms of the Words section that read better with their meaning at hand. The first
// use of each in a section carries it; common words (note, lease) are left alone.
const TERMS = [
  'nullifier',
  'commitment',
  'clearance',
  'concession',
  'bound class',
  'exit window',
  'payout account',
  'denomination',
  'request proof',
  'TVL cap',
  'set size',
  'treasury',
];
const MEANINGS = new Map<string, string>();
for (const sec of PAGE.sections) {
  for (const b of sec.blocks) {
    if (b.t !== 'table' || sec.id !== 'words') continue;
    for (const [term, meaning] of b.rows) {
      if (typeof term === 'string' && typeof meaning === 'string') {
        MEANINGS.set(term.toLowerCase(), meaning.replace(/`/g, ''));
      }
    }
  }
}
const TERM_RE = new RegExp(`\\b(${TERMS.join('|')})\\b`, 'i');

/** Opens the meaning on the side that has room. */
const place = (e: React.SyntheticEvent<HTMLElement>) => {
  const el = e.currentTarget;
  el.dataset.flip = el.getBoundingClientRect().left > window.innerWidth - 340 ? 'left' : '';
};

/** Strings carry `code` spans and, once per section, glossary terms; the rest passes through. */
function rich(node: ReactNode, seen?: Set<string>): ReactNode {
  if (typeof node !== 'string') return node;
  return node.split(/(`[^`]+`)/).flatMap((part, i): ReactNode[] => {
    if (part.startsWith('`') && part.length > 1) return [<code key={i}>{part.slice(1, -1)}</code>];
    if (!seen) return [part];
    const out: ReactNode[] = [];
    let rest = part;
    for (let guard = 0; rest && guard < 8; guard++) {
      const m = TERM_RE.exec(rest);
      const key = m?.[1]?.toLowerCase();
      if (!m || !key || seen.has(key) || !MEANINGS.has(key)) break;
      seen.add(key);
      if (m.index > 0) out.push(rest.slice(0, m.index));
      out.push(
        <dfn
          key={`${i}-${guard}`}
          className="nd-term"
          tabIndex={0}
          data-def={MEANINGS.get(key)}
          aria-label={`${m[1]}: ${MEANINGS.get(key)}`}
          onMouseEnter={place}
          onFocus={place}
        >
          {m[0]}
        </dfn>,
      );
      rest = rest.slice(m.index + m[0].length);
    }
    out.push(rest);
    return out;
  });
}

function CodeBlock({ text, label }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1400);
      })
      .catch(() => undefined);
  };
  return (
    <figure className="nd-code">
      <figcaption>
        <span>{label ?? ''}</span>
        <button type="button" onClick={copy} aria-label={`Copy ${label ?? 'code'}`}>
          {copied ? 'Copied' : 'Copy'}
        </button>
      </figcaption>
      <pre>
        <code>{text}</code>
      </pre>
    </figure>
  );
}

const slug = (t: string) =>
  t
    .toLowerCase()
    .replace(/`/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '');
/** The anchor of a sub-heading: its own id, or the section id and the heading text. */
const h3id = (sid: string, b: Extract<Block, { t: 'h3' }>) =>
  b.id ?? (typeof b.text === 'string' ? `${sid}-${slug(b.text)}` : undefined);
/** The outline of a section: its sub-headings, or, when it has none, its steps or its questions. */
const subsOf = (s: Section): { id: string; title: string }[] => {
  const heads = s.blocks.flatMap((b) => {
    const id = b.t === 'h3' ? h3id(s.id, b) : undefined;
    return b.t === 'h3' && id && typeof b.text === 'string'
      ? [{ id, title: b.text.replace(/`/g, '') }]
      : [];
  });
  if (heads.length) return heads;
  return s.blocks.flatMap((b) =>
    b.t === 'steps'
      ? b.items.map(([t]) => ({ id: `${s.id}-${slug(t)}`, title: t }))
      : b.t === 'faq'
        ? b.items.map(([q]) => ({ id: `${s.id}-${slug(q)}`, title: q }))
        : [],
  );
};

/** A sub-heading that gives its own link on hover or focus. */
function Head({ id, children }: { id?: string; children: ReactNode }) {
  const [done, setDone] = useState(false);
  return (
    <h3 id={id} className="nd-h">
      {children}
      {id && (
        <button
          type="button"
          className="nd-hlink"
          aria-label="Copy the link to this heading"
          onClick={() => {
            void navigator.clipboard
              ?.writeText(`${location.origin}${location.pathname}#docs/${id}`)
              .then(() => {
                setDone(true);
                setTimeout(() => setDone(false), 1400);
              })
              .catch(() => undefined);
          }}
        >
          {done ? 'Copied' : '#'}
        </button>
      )}
    </h3>
  );
}

const words = (n: ReactNode): number =>
  typeof n === 'string' ? n.split(/\s+/).filter(Boolean).length : 0;
/** Minutes to read a section at 200 words a minute. */
const minutesOf = (s: Section) => {
  const parts: ReactNode[] = [s.lead];
  for (const b of s.blocks) {
    if (b.t === 'p' || b.t === 'h3') parts.push(b.text);
    else if (b.t === 'list') parts.push(...b.items);
    else if (b.t === 'steps') parts.push(...b.items.flat());
    else if (b.t === 'table') parts.push(...b.rows.flat());
    else if (b.t === 'note') parts.push(b.title, b.text);
    else if (b.t === 'faq') parts.push(...b.items.flat());
    else if (b.t === 'compare') parts.push(...b.items.flatMap((i) => i.rows.map(([, v]) => v)));
  }
  const total = parts.reduce<number>((a, p) => a + words(p), 0);
  return Math.max(1, Math.round(total / 200));
};

// The sections, in the groups the side menu shows. The groups follow the page order.
const LAST_KEY = 'nymira.docs.last';
const GROUPS: [string, string[]][] = [
  ['Start', ['overview', 'try', 'judges']],
  ['Concepts', ['note-life', 'words', 'privacy']],
  ['Reference', ['web-app', 'attestation', 'vault', 'nymirad', 'agents']],
  ['Guides', ['verify', 'faq', 'develop', 'limits', 'changes']],
];

/** Which glossary terms each block of a section links: a term goes to the first block that uses it. */
function termsByBlock(section: Section): Set<string>[] {
  if (section.id === 'words') return section.blocks.map(() => new Set());
  const done = new Set<string>();
  return section.blocks.map((b) => {
    const texts =
      b.t === 'p' ? [b.text] : b.t === 'steps' ? b.items.flatMap(([t, x]) => [t, x]) : [];
    const mine = new Set<string>();
    for (const text of texts) {
      if (typeof text !== 'string') continue;
      for (const term of TERMS) {
        const key = term.toLowerCase();
        if (!done.has(key) && new RegExp(`\\b${term}\\b`, 'i').test(text.replace(/`[^`]*`/g, ''))) {
          done.add(key);
          mine.add(key);
        }
      }
    }
    return mine;
  });
}

function Render({
  block,
  sid,
  allowed,
}: {
  block: Block;
  sid: string;
  allowed: Set<string>;
}): ReactNode {
  // A fresh set on every call keeps the render pure: it only marks what this block may not link.
  const seen = new Set(TERMS.map((t) => t.toLowerCase()).filter((t) => !allowed.has(t)));
  switch (block.t) {
    case 'p':
      return <p>{rich(block.text, seen)}</p>;
    case 'h3':
      return <Head id={h3id(sid, block)}>{rich(block.text)}</Head>;
    case 'list':
      return (
        <ul className="nd-list">
          {block.items.map((item, i) => (
            <li key={i}>{rich(item)}</li>
          ))}
        </ul>
      );
    case 'steps':
      return (
        <ol className="nd-steps">
          {block.items.map(([title, text], i) => (
            <li key={i} style={vars({ '--i': i + 1 })}>
              <Head id={`${sid}-${slug(title)}`}>{title}</Head>
              <p>{rich(text, seen)}</p>
            </li>
          ))}
        </ol>
      );
    case 'table':
      return (
        <div className="nd-table" role="region" aria-label={block.label} tabIndex={0}>
          <table>
            <thead>
              <tr>
                {block.head.map((h) => (
                  <th key={h} scope="col">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, i) => (
                <tr key={i}>
                  {row.map((cell, j) => (
                    <td key={j}>{rich(cell)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case 'code':
      return <CodeBlock text={block.text} label={block.label} />;
    case 'note':
      return (
        <aside className="nd-note">
          <strong>{block.title}</strong>
          <p>{rich(block.text)}</p>
        </aside>
      );
    case 'diagram':
      return block.id === 'parties' ? (
        <Parties />
      ) : block.id === 'lease' ? (
        <Lease />
      ) : block.id === 'exit' ? (
        <ExitWindow />
      ) : (
        <NoteLife />
      );
    case 'calc':
      return <ChargeCalc />;
    case 'calendar':
      return <NoteCalendar />;
    case 'vault-compare':
      return <VaultCompare />;
    case 'ip':
      return <IpMatrix rich={rich} />;
    case 'compare':
      return (
        <div className="nd-compare">
          {block.items.map((item) => (
            <article key={item.name}>
              <h3>{item.name}</h3>
              <dl>
                {item.rows.map(([k, v]) => (
                  <div key={k}>
                    <dt>{k}</dt>
                    <dd>{rich(v)}</dd>
                  </div>
                ))}
              </dl>
            </article>
          ))}
        </div>
      );
    case 'faq':
      return (
        <div className="nd-faq">
          {block.items.map(([q, a]) => (
            <div key={q}>
              <Head id={`${sid}-${slug(q)}`}>{q}</Head>
              <p>{rich(a)}</p>
            </div>
          ))}
        </div>
      );
  }
}

function Links({
  links,
  onJump,
  side = false,
}: {
  links: DocLink[];
  onJump: (id: string) => void;
  side?: boolean;
}) {
  return (
    <nav className={side ? 'nd-links nd-links-side' : 'nd-links'} aria-label="Read more">
      {links.map((l) =>
        l.ext ? (
          <a key={l.href} href={l.href} target="_blank" rel="noopener noreferrer">
            {l.label}
            <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <path
                d="M4.5 11.5l7-7M5 4.5h6.5V11"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
              />
            </svg>
          </a>
        ) : (
          <a
            key={l.href}
            href={l.href}
            onClick={(e) => {
              if (l.href.startsWith('#docs/')) {
                e.preventDefault();
                onJump(l.href.slice(6));
              }
            }}
          >
            {l.label}
          </a>
        ),
      )}
    </nav>
  );
}

function Article({
  section,
  prev,
  next,
  onJump,
}: {
  section: Section;
  prev?: Section;
  next?: Section;
  onJump: (id: string) => void;
}) {
  const allowed = termsByBlock(section);
  return (
    <section id={section.id} className="nd-section" aria-labelledby={`${section.id}-title`}>
      <h2 id={`${section.id}-title`} tabIndex={-1}>
        <a
          href={`#docs/${section.id}`}
          aria-label={`Link to ${section.title}`}
          onClick={(e) => {
            e.preventDefault();
            onJump(section.id);
          }}
        >
          {section.title}
        </a>
      </h2>
      <div className="nd-section-body">
        {section.lead && <p className="nd-lead">{rich(section.lead)}</p>}
        {section.blocks.map((block, i) => (
          <Render key={i} block={block} sid={section.id} allowed={allowed[i] ?? new Set()} />
        ))}
        {section.links && <Links links={section.links} onJump={onJump} />}
        <nav className="nd-pager" aria-label="Sections">
          {prev ? (
            <a
              href={`#docs/${prev.id}`}
              rel="prev"
              onClick={(e) => {
                e.preventDefault();
                onJump(prev.id);
              }}
            >
              <small>Previous</small>
              {prev.title}
            </a>
          ) : (
            <span />
          )}
          {next && (
            <a
              href={`#docs/${next.id}`}
              rel="next"
              data-next="true"
              onClick={(e) => {
                e.preventDefault();
                onJump(next.id);
              }}
            >
              <small>Next</small>
              {next.title}
            </a>
          )}
        </nav>
      </div>
    </section>
  );
}

/**
 * Where the reader is inside the section on screen: how far down it they are (the section's cell
 * fills as they go, written straight onto it so scrolling does not render the page) and which
 * sub-heading they are under.
 */
function useReading(current: string, subs: string[]) {
  const [sub, setSub] = useState('');
  useEffect(() => {
    let frame = 0;
    const measure = () => {
      frame = 0;
      const line = Math.min(window.innerHeight * 0.35, 320);
      const max = document.documentElement.scrollHeight - window.innerHeight;
      const done = max > 0 ? Math.min(1, window.scrollY / max) : 1;
      for (const host of document.querySelectorAll<HTMLElement>(`[data-cell="${current}"]`)) {
        host.style.setProperty('--fill', done.toFixed(3));
      }
      let inside = '';
      for (const id of subs) {
        const el = document.getElementById(id);
        if (el && el.getBoundingClientRect().top <= line) inside = id;
      }
      setSub(inside);
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };
    measure();
    window.addEventListener('scroll', onScroll, { passive: true });
    window.addEventListener('resize', onScroll);
    return () => {
      window.removeEventListener('scroll', onScroll);
      window.removeEventListener('resize', onScroll);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [current, subs]);
  return sub;
}

// ---- search over the page text ----
interface Hit {
  section: Section;
  heading: string;
  text: string;
  anchor?: string;
}
const flat = (n: ReactNode): string => (typeof n === 'string' ? n.replace(/`/g, '') : '');

function buildIndex(): Hit[] {
  const out: Hit[] = [];
  for (const section of PAGE.sections) {
    let heading = section.title;
    let anchor: string | undefined;
    const add = (text: string) => {
      if (text.trim()) out.push({ section, heading, text, anchor });
    };
    add(flat(section.lead));
    for (const b of section.blocks) {
      switch (b.t) {
        case 'h3':
          heading = flat(b.text);
          anchor = h3id(section.id, b);
          add(heading);
          break;
        case 'p':
          add(flat(b.text));
          break;
        case 'list':
          b.items.forEach((i) => add(flat(i)));
          break;
        case 'steps':
          b.items.forEach(([t, x]) => add(`${t}. ${flat(x)}`));
          break;
        case 'table':
          b.rows.forEach((r) => add(r.map(flat).join(' — ')));
          break;
        case 'note':
          add(`${b.title}. ${flat(b.text)}`);
          break;
        case 'faq':
          b.items.forEach(([q, a]) => add(`${q} ${flat(a)}`));
          break;
        case 'code':
          add(`${b.label ?? ''} ${b.text}`);
          break;
        default:
      }
    }
  }
  return out;
}

function snippet(text: string, word: string | undefined): string {
  const at = Math.max(0, text.toLowerCase().indexOf(word ?? ''));
  const from = Math.max(0, at - 36);
  const end = from + 150;
  return `${from > 0 ? '…' : ''}${text.slice(from, end)}${text.length > end ? '…' : ''}`;
}

/** The search words, marked in a piece of text. */
function marked(text: string, ws: string[]): ReactNode {
  if (!ws.length) return text;
  const re = new RegExp(
    `(${ws.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`,
    'gi',
  );
  return text.split(re).map((part, i) => (i % 2 ? <mark key={i}>{part}</mark> : part));
}

function Search({ onClose, onGo }: { onClose: () => void; onGo: (id: string) => void }) {
  const index = useMemo(buildIndex, []);
  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const [group, setGroup] = useState('All');
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), []);
  const words = useMemo(() => q.toLowerCase().split(/\s+/).filter(Boolean), [q]);
  const hits = useMemo(() => {
    if (!words.length) return [];
    const seen = new Set<string>();
    const scored: [number, Hit][] = [];
    const members = group === 'All' ? null : (GROUPS.find(([g]) => g === group)?.[1] ?? []);
    for (const h of index) {
      if (members && !members.includes(h.section.id)) continue;
      const hay = `${h.section.title} ${h.heading} ${h.text}`.toLowerCase();
      if (!words.every((w) => hay.includes(w))) continue;
      const key = `${h.section.id}|${h.text.slice(0, 40)}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const head = `${h.section.title} ${h.heading}`.toLowerCase();
      scored.push([words.filter((w) => head.includes(w)).length, h]);
    }
    return scored
      .sort((a, b) => b[0] - a[0])
      .slice(0, 8)
      .map(([, h]) => h);
  }, [words, index, group]);
  const go = (h: Hit | undefined) => {
    if (!h) return;
    onClose();
    onGo(h.anchor ?? h.section.id);
  };
  return (
    <div
      className="nd-search"
      role="dialog"
      aria-modal="true"
      aria-label="Search the documentation"
      onMouseDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div className="nd-search-box">
        <input
          ref={input}
          value={q}
          placeholder="Search the documentation"
          aria-label="Search the documentation"
          spellCheck={false}
          onChange={(e) => {
            setQ(e.target.value);
            setSel(0);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Escape') onClose();
            else if (e.key === 'ArrowDown') {
              e.preventDefault();
              setSel((s) => Math.min(s + 1, hits.length - 1));
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              setSel((s) => Math.max(s - 1, 0));
            } else if (e.key === 'Enter') go(hits[sel]);
          }}
        />
        <div className="nd-search-groups" role="group" aria-label="Where to look">
          {['All', ...GROUPS.map(([g]) => g)].map((g) => (
            <button
              key={g}
              type="button"
              aria-pressed={group === g}
              onClick={() => {
                setGroup(g);
                setSel(0);
                input.current?.focus();
              }}
            >
              {g}
            </button>
          ))}
        </div>
        <ul role="listbox" aria-label="Results">
          {hits.map((h, i) => (
            <li key={`${h.section.id}${i}`} role="option" aria-selected={i === sel}>
              <button type="button" onMouseEnter={() => setSel(i)} onClick={() => go(h)}>
                <b>
                  {marked(h.section.title, words)}
                  {h.heading !== h.section.title ? <> · {marked(h.heading, words)}</> : null}
                </b>
                <span>{marked(snippet(h.text, words[0]), words)}</span>
              </button>
            </li>
          ))}
          {words.length > 0 && hits.length === 0 && <li className="nd-search-none">No match.</li>}
        </ul>
        <p className="nd-search-hint">
          <kbd>↑</kbd> <kbd>↓</kbd> to move, <kbd>Enter</kbd> to open, <kbd>Esc</kbd> to close
        </p>
      </div>
    </div>
  );
}

function CopyLink() {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      onClick={() => {
        void navigator.clipboard
          ?.writeText(location.href)
          .then(() => {
            setDone(true);
            setTimeout(() => setDone(false), 1400);
          })
          .catch(() => undefined);
      }}
    >
      {done ? 'Link copied' : 'Copy link to this page'}
    </button>
  );
}

export function Docs({
  theme,
  onTheme,
  onHome,
  onOpenApp,
  onStatus,
}: {
  theme: 'dark' | 'light';
  onTheme: () => void;
  onHome: () => void;
  onOpenApp: () => void;
  onStatus: () => void;
}) {
  const ids = useMemo(() => PAGE.sections.map((s) => s.id), []);
  const subMap = useMemo(
    () => Object.fromEntries(PAGE.sections.map((s) => [s.id, subsOf(s).map((x) => x.id)])),
    [],
  );
  // One section at a time. A link such as #docs/privacy, or one to a sub-heading, opens its section.
  const sectionOf = (id: string) =>
    ids.includes(id) ? id : (ids.find((sid) => subMap[sid]?.includes(id)) ?? ids[0]!);
  const start = () => {
    const target = location.hash.split('/')[1];
    return target ? sectionOf(target) : ids[0]!;
  };
  const [active, setActive] = useState(start);
  const [visited, setVisited] = useState(() => new Set([active]));
  const [raw, setRaw] = useState(false);
  const [announce, setAnnounce] = useState('');
  const moved = useRef(false);
  // Where the reader stopped last time, offered at the top of the first page.
  const [resume] = useState(() => {
    try {
      const last = localStorage.getItem(LAST_KEY);
      return !location.hash.split('/')[1] && last && last !== ids[0] && ids.includes(last)
        ? last
        : null;
    } catch {
      return null;
    }
  });
  const sub = useReading(active, subMap[active] ?? []);
  const [find, setFind] = useState(false);
  const stripRef = useRef<HTMLElement>(null);
  const anchor = useRef<string | null>(null);

  // The browser must not put the page back where a reload left it.
  useEffect(() => {
    const before = history.scrollRestoration;
    history.scrollRestoration = 'manual';
    return () => {
      history.scrollRestoration = before;
    };
  }, []);

  // A section that opens is remembered, announced to a screen reader, and takes the focus.
  useEffect(() => {
    try {
      localStorage.setItem(LAST_KEY, active);
    } catch {
      // Not remembered.
    }
    if (!moved.current) {
      moved.current = true;
      return;
    }
    const title = PAGE.sections.find((x) => x.id === active)?.title ?? '';
    setAnnounce(`${title}, page ${ids.indexOf(active) + 1} of ${ids.length}`);
    document.getElementById(`${active}-title`)?.focus({ preventScroll: true });
  }, [active, ids]);

  // The back and forward buttons turn the pages too.
  useEffect(() => {
    const onPop = () => {
      if (!location.hash.startsWith('#docs')) return;
      const t = location.hash.split('/')[1];
      const sec = t ? sectionOf(t) : ids[0]!;
      anchor.current = t && t !== sec ? t : null;
      setVisited((v) => new Set(v).add(sec));
      setActive(sec);
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // After a section opens: to its top, or to the sub-heading that was asked for.
  useEffect(() => {
    const id = anchor.current ?? location.hash.split('/')[1];
    anchor.current = null;
    const el = id && id !== active ? document.getElementById(id) : null;
    if (!el) {
      window.scrollTo({ top: 0, behavior: 'instant' });
      return;
    }
    // The section has just mounted: wait for its layout.
    const t = setTimeout(() => {
      el.scrollIntoView({ block: 'start', behavior: 'instant' });
      window.dispatchEvent(new Event('scroll'));
    }, 120);
    return () => clearTimeout(t);
  }, [active]);

  // Search opens with "/" or Ctrl/Cmd+K, unless the reader is typing somewhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test((e.target as HTMLElement)?.tagName ?? '');
      if ((e.key === 'k' && (e.metaKey || e.ctrlKey)) || (e.key === '/' && !typing)) {
        e.preventDefault();
        setFind(true);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // On a phone the strip scrolls with the reader: keep the current cell in view.
  useEffect(() => {
    stripRef.current
      ?.querySelector<HTMLElement>('[aria-current="true"]')
      ?.scrollIntoView({ block: 'nearest', inline: 'center' });
  }, [active]);

  const jump = (id: string) => {
    const sec = sectionOf(id);
    if (sec === active) history.replaceState(null, '', `#docs/${id}`);
    else history.pushState(null, '', `#docs/${id}`);
    if (sec === active) {
      const el = id === sec ? null : document.getElementById(id);
      if (el) el.scrollIntoView({ block: 'start', behavior: 'smooth' });
      else window.scrollTo({ top: 0, behavior: 'smooth' });
      return;
    }
    anchor.current = id;
    setVisited((v) => new Set(v).add(sec));
    setActive(sec);
  };

  // Left and right arrows turn the page.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (
        e.altKey ||
        e.ctrlKey ||
        e.metaKey ||
        /^(INPUT|TEXTAREA|SELECT)$/.test(t?.tagName ?? '')
      ) {
        return;
      }
      const i = ids.indexOf(active);
      const to =
        e.key === 'ArrowRight' ? ids[i + 1] : e.key === 'ArrowLeft' ? ids[i - 1] : undefined;
      if (to) jump(to);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  const current = PAGE.sections.find((s) => s.id === active);
  const at = ids.indexOf(active);
  const seen = (id: string) =>
    id !== active && visited.has(id) ? vars({ '--fill': 1 }) : undefined;

  return (
    <div className="nm-site nm-app nd" data-theme={theme} data-testid="docs">
      <a
        className="nd-skip"
        href="#docs-content"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById('docs-content')?.focus();
        }}
      >
        Skip to the page
      </a>
      <div className="nd-sr" role="status" aria-live="polite">
        {announce}
      </div>
      <header className="nd-header">
        <a
          className="nd-brand"
          href="#home"
          aria-label="Nymira home"
          draggable={false}
          onClick={(e) => {
            e.preventDefault();
            onHome();
          }}
        >
          <Wordmark />
        </a>
        <span className="nd-where">Docs</span>
        <span className="nd-spacer" />
        <button type="button" className="nd-find" onClick={() => setFind(true)}>
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <circle cx="7" cy="7" r="4.5" stroke="currentColor" strokeWidth="1.3" />
            <path d="M10.5 10.5L14 14" stroke="currentColor" strokeWidth="1.3" />
          </svg>
          <span>Search</span>
          <kbd aria-hidden="true">/</kbd>
        </button>
        <button type="button" className="nd-find nd-status-link" onClick={onStatus}>
          Status
        </button>
        <ThemeToggle theme={theme} onTheme={onTheme} />
        <button type="button" className="nd-button" onClick={onOpenApp}>
          Open the app
        </button>
      </header>

      <nav className="nd-strip" aria-label="On this page" ref={stripRef}>
        <span className="nd-strip-now">{current?.title}</span>
        <ol>
          {PAGE.sections.map((s) => (
            <li key={s.id}>
              <a
                href={`#docs/${s.id}`}
                data-cell={s.id}
                style={seen(s.id)}
                aria-current={active === s.id ? 'true' : undefined}
                aria-label={s.title}
                onClick={(e) => {
                  e.preventDefault();
                  jump(s.id);
                }}
              />
            </li>
          ))}
        </ol>
      </nav>

      <div className="nd-body">
        <nav className="nd-rail" aria-label="On this page">
          {GROUPS.map(([label, members]) => (
            <div key={label} className="nd-rail-group">
              <span>{label}</span>
              <ol>
                {members.map((id) => {
                  const s = PAGE.sections.find((x) => x.id === id);
                  return s ? (
                    <li key={s.id}>
                      <a
                        href={`#docs/${s.id}`}
                        aria-current={active === s.id ? 'true' : undefined}
                        onClick={(e) => {
                          e.preventDefault();
                          jump(s.id);
                        }}
                      >
                        <i data-cell={s.id} style={seen(s.id)} aria-hidden="true" />
                        {s.title}
                      </a>
                    </li>
                  ) : null;
                })}
              </ol>
            </div>
          ))}
        </nav>
        <main className="nd-main" id="docs-content" tabIndex={-1}>
          {at === 0 && (
            <div className="nd-hero">
              <h1>{PAGE.title}</h1>
              <p className="nd-lead">{rich(PAGE.lead)}</p>
              <p className="nd-status" data-testid="docs-status">
                <span>{PROFILE.network}</span>
                <span>test funds only</span>
              </p>
              {resume && (
                <button type="button" className="nd-resume" onClick={() => jump(resume)}>
                  <small>Continue where you left off</small>
                  {PAGE.sections.find((x) => x.id === resume)?.title}
                </button>
              )}
            </div>
          )}
          {current && (
            <Article
              key={current.id}
              section={current}
              prev={PAGE.sections[at - 1]}
              next={PAGE.sections[at + 1]}
              onJump={jump}
            />
          )}
          <footer className="nd-foot">
            <span>Nymira docs · {PROFILE.network} · release 0.1.3 · test funds only</span>
            <nav aria-label="Project">
              <a
                href="#status"
                onClick={(e) => {
                  e.preventDefault();
                  onStatus();
                }}
              >
                Status
              </a>
              <a
                href="https://github.com/nymiralabs/nymira"
                target="_blank"
                rel="noopener noreferrer"
              >
                GitHub
              </a>
              <a
                href="https://github.com/nymiralabs/nymira/releases"
                target="_blank"
                rel="noopener noreferrer"
              >
                Releases
              </a>
              <a
                href="https://github.com/nymiralabs/nymira/blob/main/THREAT_MODEL.md"
                target="_blank"
                rel="noopener noreferrer"
              >
                Threat model
              </a>
              <a
                href="https://github.com/nymiralabs/nymira/blob/main/TERMS.md"
                target="_blank"
                rel="noopener noreferrer"
              >
                Terms
              </a>
            </nav>
          </footer>
        </main>
        {current && (
          <aside className="nd-aside" aria-label="About this page">
            {subsOf(current).length > 0 && (
              <div className="nd-aside-block">
                <span className="nd-aside-title">On this page</span>
                <ol className="nd-outline">
                  {subsOf(current).map((x) => (
                    <li key={x.id}>
                      <a
                        href={`#docs/${x.id}`}
                        aria-current={sub === x.id ? 'true' : undefined}
                        onClick={(e) => {
                          e.preventDefault();
                          jump(x.id);
                        }}
                      >
                        {x.title}
                      </a>
                    </li>
                  ))}
                </ol>
              </div>
            )}
            <div className="nd-aside-block nd-aside-meta">
              <span>{minutesOf(current)} min read</span>
              <span>
                Page {at + 1} of {ids.length}
              </span>
            </div>
            <LivePanel onStatus={onStatus} />
            <div className="nd-aside-block nd-aside-tools">
              <CopyLink />
              <button type="button" onClick={() => setRaw(true)}>
                View as Markdown
              </button>
              <a
                href={`https://github.com/nymiralabs/nymira/issues/new?title=${encodeURIComponent(`Docs: ${current.title}`)}`}
                target="_blank"
                rel="noopener noreferrer"
              >
                Report a problem with this page
              </a>
              <a
                href="https://github.com/nymiralabs/nymira/blob/main/web/src/ui/docs-content.tsx"
                target="_blank"
                rel="noopener noreferrer"
              >
                Suggest an edit
              </a>
            </div>
            {current.links && (
              <div className="nd-aside-block">
                <span className="nd-aside-title">Related</span>
                <Links side links={current.links} onJump={jump} />
              </div>
            )}
          </aside>
        )}
      </div>
      {find && <Search onClose={() => setFind(false)} onGo={jump} />}
      {raw && current && <RawView section={current} onClose={() => setRaw(false)} />}
    </div>
  );
}
