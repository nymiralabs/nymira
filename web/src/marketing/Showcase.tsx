import { useEffect, useRef, useState } from 'react';
import { Board } from './Board';
import { Icon, type IconName } from './Icon';
import {
  CLI_STAGES,
  CliScene,
  MCP_STAGES,
  McpScene,
  SOURCE_STAGES,
  SourceScene,
  hotSpot,
} from './Scenes';
import { TypedCommand } from './Visuals';
import './showcase.css';

const TABS: readonly {
  id: string;
  name: string;
  blurb: string;
  icon: IconName;
  stages: number;
  title: string;
  note: string;
}[] = [
  {
    id: 'cli',
    name: 'CLI',
    blurb: 'Run nymirad on your machine',
    icon: 'terminal',
    stages: CLI_STAGES,
    title: 'One conversation',
    note: 'Illustration · example data',
  },
  {
    id: 'mcp',
    name: 'MCP',
    blurb: 'Give an agent a budget',
    icon: 'plug',
    stages: MCP_STAGES,
    title: 'One agent session',
    note: 'Illustration · example data',
  },
  {
    id: 'source',
    name: 'Source',
    blurb: 'Rebuild it and compare hashes',
    icon: 'branch',
    stages: SOURCE_STAGES,
    title: 'Two checks',
    note: 'Testnet release values',
  },
];

export function Showcase({ source }: { source: string }) {
  const commands = ['nymirad serve', 'nymira_balance', `git clone ${source}.git`];
  const [pos, setPos] = useState({ tab: 0, stage: 0 });
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const manual = useRef(false);
  const container = useRef<HTMLDivElement>(null);
  const reduced =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  useEffect(() => {
    const element = container.current;
    if (!element || reduced) return;
    let visible = false;
    const observer = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? false;
    });
    observer.observe(element);
    const timer = setInterval(() => {
      if (!visible || document.hidden) return;
      if (element.matches(':hover') || element.querySelector(':focus-visible')) return;
      setPos((p) => {
        if (p.stage + 1 < TABS[p.tab]!.stages) return { tab: p.tab, stage: p.stage + 1 };
        if (manual.current) return { tab: p.tab, stage: 0 };
        return { tab: (p.tab + 1) % TABS.length, stage: 0 };
      });
    }, 1400);
    return () => {
      clearInterval(timer);
      observer.disconnect();
    };
  }, [reduced]);

  const tab = TABS[pos.tab]!;
  const stage = reduced ? tab.stages - 1 : pos.stage;
  const choose = (i: number) => {
    manual.current = true;
    setPos({ tab: i, stage: 0 });
    setCopied(false);
    setCopyError(false);
  };
  async function copy() {
    manual.current = true;
    try {
      await navigator.clipboard.writeText(commands[pos.tab]!);
      setCopied(true);
      setCopyError(false);
    } catch {
      setCopyError(true);
    }
  }

  return (
    <div className="nm-command-block nm-show" ref={container}>
      <div className="nm-command">
        <code>
          <span>{pos.tab === 1 ? '> ' : '$ '}</span>
          <TypedCommand text={commands[pos.tab]!} />
          <span className="nm-cursor" />
        </code>
        <button type="button" onClick={copy} aria-label="Copy command">
          <Icon name={copied ? 'check' : 'copy'} size={18} />
        </button>
      </div>
      <div className="nm-show-tabs" role="tablist" aria-label="Choose an interface">
        {TABS.map((t, i) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`nm-tab-${t.id}`}
            aria-selected={pos.tab === i}
            aria-controls="nm-show-panel"
            tabIndex={pos.tab === i ? 0 : -1}
            onClick={() => choose(i)}
            onKeyDown={(e) => {
              const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
              if (!step) return;
              e.preventDefault();
              choose((i + step + TABS.length) % TABS.length);
              (
                e.currentTarget.parentElement?.children[(i + step + TABS.length) % TABS.length] as
                  HTMLElement | undefined
              )?.focus();
            }}
          >
            <Icon name={t.icon} size={22} />
            <span>
              <strong>{t.name}</strong>
              <small>{t.blurb}</small>
            </span>
            <i
              aria-hidden="true"
              style={{ width: pos.tab === i ? `${((stage + 1) / t.stages) * 100}%` : 0 }}
            />
          </button>
        ))}
      </div>
      <div className="nm-demo-bar">
        <span>{tab.title}</span>
        <span>{tab.note}</span>
      </div>
      <Board id={tab.id} hot={hotSpot(pos.tab, stage)}>
        {pos.tab === 0 && <CliScene s={stage} />}
        {pos.tab === 1 && <McpScene s={stage} />}
        {pos.tab === 2 && <SourceScene s={stage} />}
      </Board>
      <span className="nm-sr-only" role="status">
        {copyError
          ? 'Copy failed. Select the command to copy it manually.'
          : copied
            ? 'Command copied.'
            : ''}
      </span>
    </div>
  );
}
