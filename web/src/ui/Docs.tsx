import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Wordmark } from '../marketing/Wordmark';
import { ThemeToggle } from '../marketing/ThemeToggle';
import { PROFILE } from '../profile';
import { NoteLife } from './docs-diagrams';
import { PAGE, type Block, type Section } from './docs-content';
import './docs.css';

/** Strings carry `code` spans and the {{new}} tag; everything else passes through. */
function rich(node: ReactNode): ReactNode {
  if (typeof node !== 'string') return node;
  return node.split(/(`[^`]+`|\{\{new\}\})/).map((part, i) => {
    if (part === '{{new}}') {
      return (
        <em key={i} className="nd-new">
          not in 0.1.2
        </em>
      );
    }
    if (part.startsWith('`')) return <code key={i}>{part.slice(1, -1)}</code>;
    return part;
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

function Render({ block }: { block: Block }): ReactNode {
  switch (block.t) {
    case 'p':
      return <p>{rich(block.text)}</p>;
    case 'h3':
      return <h3 id={block.id}>{rich(block.text)}</h3>;
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
            <li key={i}>
              <h3>{title}</h3>
              <p>{rich(text)}</p>
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
      return <NoteLife />;
  }
}

function Article({ section }: { section: Section }) {
  return (
    <section id={section.id} className="nd-section" aria-labelledby={`${section.id}-title`}>
      <h2 id={`${section.id}-title`}>{section.title}</h2>
      {section.lead && <p className="nd-lead">{rich(section.lead)}</p>}
      {section.blocks.map((block, i) => (
        <Render key={i} block={block} />
      ))}
    </section>
  );
}

/** Which section the reader is in: the last one whose top has passed the line under the header. */
function useActive(ids: string[]) {
  const [active, setActive] = useState(ids[0] ?? '');
  useEffect(() => {
    const onScroll = () => {
      let current = ids[0] ?? '';
      for (const id of ids) {
        const el = document.getElementById(id);
        if (el && el.getBoundingClientRect().top <= 140) current = id;
      }
      setActive(current);
    };
    onScroll();
    window.addEventListener('scroll', onScroll, { passive: true });
    return () => window.removeEventListener('scroll', onScroll);
  }, [ids]);
  return active;
}

export function Docs({
  theme,
  onTheme,
  onHome,
  onOpenApp,
}: {
  theme: 'dark' | 'light';
  onTheme: () => void;
  onHome: () => void;
  onOpenApp: () => void;
}) {
  const ids = PAGE.sections.map((s) => s.id);
  const active = useActive(ids);
  const tocRef = useRef<HTMLElement>(null);

  // A link such as #docs/privacy opens on that section.
  useEffect(() => {
    const target = location.hash.split('/')[1];
    if (target && ids.includes(target)) {
      requestAnimationFrame(() =>
        document.getElementById(target)?.scrollIntoView({ block: 'start' }),
      );
    } else {
      window.scrollTo({ top: 0, behavior: 'auto' });
    }
    // Only on the first render of the page.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // On a phone the contents row scrolls sideways: keep the current entry in view.
  useEffect(() => {
    const el = tocRef.current?.querySelector<HTMLElement>('[aria-current="true"]');
    el?.scrollIntoView({ block: 'nearest', inline: 'center' });
  }, [active]);

  const jump = (id: string) => {
    history.replaceState(null, '', `#docs/${id}`);
    document.getElementById(id)?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  };

  return (
    <div className="nm-site nm-app nd" data-theme={theme} data-testid="docs">
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
        <ThemeToggle theme={theme} onTheme={onTheme} />
        <button type="button" className="nd-button" onClick={onOpenApp}>
          Open the app
        </button>
      </header>
      <div className="nd-body">
        <nav className="nd-toc" aria-label="On this page" ref={tocRef}>
          <span className="nd-toc-label">On this page</span>
          <ol>
            {PAGE.sections.map((s) => (
              <li key={s.id}>
                <a
                  href={`#docs/${s.id}`}
                  aria-current={active === s.id ? 'true' : undefined}
                  onClick={(e) => {
                    e.preventDefault();
                    jump(s.id);
                  }}
                >
                  <i aria-hidden="true" />
                  {s.title}
                </a>
              </li>
            ))}
          </ol>
        </nav>
        <main className="nd-main">
          <h1>{PAGE.title}</h1>
          <p className="nd-lead">{rich(PAGE.lead)}</p>
          <p className="nd-status" data-testid="docs-status">
            <span>{PROFILE.network}</span>
            <span>test funds only</span>
          </p>
          {PAGE.sections.map((s) => (
            <Article key={s.id} section={s} />
          ))}
        </main>
      </div>
    </div>
  );
}
