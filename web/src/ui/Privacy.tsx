// The privacy panel: what anyone can read on chain about a note (left) beside what the operator
// saw for each lease on it (right), with the set size of every lease. The left column comes from
// the deposit record this browser keeps and makes no request. The set size of a lease was counted
// when the lease opened and is stored with it; the "now" figure reads the vault's views again.
// Between them: what joins the two views, the reuse check, what leaving shows and who sees what.
import { useEffect, useMemo, useState } from 'react';
import { CLOSE_DAYS } from '../leave/guard';
import './privacy.css';
import { Icon } from '../marketing/Icon';
import { Button, Card, Chip, Status, WaitBar, Waiting } from '../marketing/kit';
import type { NoteStatus } from '../marketing/kit';
import {
  FLAG_NAME,
  Journey,
  SetBars,
  SetGauge,
  SetCells,
  SetLegend,
  Timeline,
  type Flag,
  type SetBar,
  type Stage,
  type Stop,
} from '../marketing/kit-privacy';
import { DEFAULT_BOUND } from '../chat/models';
import { OperatorClient, OperatorError, OperatorUnreachable } from '../chat/operator';
import { INTENTS_SIGNER } from '../funding/intents';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { loadNotes } from '../funding/flow';
import { REQUEST_LIFETIME_SECS } from '../note/lease';
import type { ActiveLease, LeaseRecord, NoteView } from '../note/lease';
import { coverOf, small, thin } from '../privacy/cover';
import {
  differences,
  evaluate,
  headline,
  isFigure,
  readNow,
  teamOf,
  type SetSize,
} from '../privacy/setsize';
import { fill, sentence, type StringKey } from '../privacy/strings';
import * as text from '../privacy/text';
import { PROFILE } from '../profile';
import { noteAad, type NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { formatDate, formatUnits } from './format';
import { setSizeRpcs } from './services';
import { Unlock } from './Unlock';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;
const utc = (secs: number) =>
  new Date(secs * 1000).toISOString().replace('T', ' ').slice(0, 19) + ' UTC';
const pinned = () => PROFILE.pins?.teamNoteIds ?? [];

type Now =
  { status: 'loading' } | { status: 'ok'; size: SetSize } | { status: 'failed'; why: string };

const slug = (id: string | number) => `lease-${String(id).replace(/[^a-z0-9_-]/gi, '')}`;

type LeaseItem = { record: LeaseRecord; open: boolean };

function flagsOf(record: LeaseRecord, figure: SetSize | undefined, thinCover: boolean): Flag[] {
  const out: Flag[] = [];
  if (BigInt(record.boundClass) > DEFAULT_BOUND) {
    out.push({ kind: 'stands', text: text.STANDS_OUT(units(record.boundClass)) });
  }
  if (figure && small(figure.external)) out.push({ kind: 'small', text: text.SMALL_SET });
  if (thinCover) out.push({ kind: 'timing', text: text.TIMING_WARNING });
  return out;
}

const FLAG_TESTID = { stands: 'stands-out', small: 'small-set', timing: 'timing-warning' } as const;

function useLeaseView(record: LeaseRecord, note: NoteRecord) {
  const figure = isFigure(record.setSize) ? record.setSize : undefined;
  const team = teamOf(pinned(), figure?.operatorTeam ?? null);
  const cover = coverOf(
    note.noteId ?? 0,
    Math.floor((note.depositedAt ?? note.createdAt) / 1000),
    record,
    team,
  );
  return { figure, cover, flags: flagsOf(record, figure, thin(cover)) };
}

/** One lease, in full: what the operator recorded, how many notes could have paid, what to watch. */
function LeaseRow({ record, note, open }: LeaseItem & { note: NoteRecord }) {
  const { figure, cover, flags } = useLeaseView(record, note);
  const s = record.sampling;
  const sampling = [
    s.temperature !== undefined ? `temperature ${s.temperature}` : null,
    s.top_p !== undefined ? `top_p ${s.top_p}` : null,
    s.stream ? 'streamed' : 'not streamed',
  ]
    .filter(Boolean)
    .join(', ');
  return (
    <li
      className="nc-detail"
      id={slug(record.leaseId || record.openedAt)}
      data-testid="lease-row"
      data-lease={record.leaseId}
    >
      <header>
        <h3 data-testid="lease-time">{utc(record.openedAt)}</h3>
        {open && <Chip tone="live">still open</Chip>}
        <code data-testid="lease-id">{record.leaseId}</code>
      </header>
      <div className="nc-set">
        {figure ? (
          <>
            <p className="nc-set-head">
              <strong>{figure.external}</strong>
              <span data-testid="set-size">{headline(figure)}</span>
            </p>
            <SetCells
              ext={figure.external}
              team={figure.total - figure.external}
              label={`${figure.external} external notes could have paid`}
            />
            <SetLegend />
            {differences(figure).map((d) => (
              <p key={d} className="nc-cap" data-testid="set-difference">
                {d}
              </p>
            ))}
            {!small(figure.external) && (
              <p className="nc-cap" data-testid="five-not-safe">
                {sentence('five_not_safe')}
              </p>
            )}
          </>
        ) : (
          <p className="nc-cap nc-cap-first" data-testid="set-size-missing">
            {record.setSize && 'unavailable' in record.setSize
              ? fill('unavailable', { why: record.setSize.unavailable })
              : text.NO_RECORD}
          </p>
        )}
      </div>
      {flags.length > 0 && (
        <ul className="nc-flaglist">
          {flags.map((f) => (
            <li key={f.kind}>
              <Chip tone="warn">{FLAG_NAME[f.kind]}</Chip>
              <span data-testid={FLAG_TESTID[f.kind]}>{f.text}</span>
            </li>
          ))}
        </ul>
      )}
      <dl className="nc-facts">
        <div>
          <dt>Bound class</dt>
          <dd data-testid="lease-bound">{units(record.boundClass)}</dd>
        </div>
        <div>
          <dt>Charge</dt>
          <dd data-testid="lease-charge">
            {record.charge !== undefined ? units(record.charge) : 'not settled yet'}
          </dd>
        </div>
        <div>
          <dt>Model</dt>
          <dd>{record.model || 'none yet'}</dd>
        </div>
        <div>
          <dt>Settings</dt>
          <dd>{sampling}</dd>
        </div>
        <div>
          <dt>Messages</dt>
          <dd>
            {record.messageCount} messages, {record.messageBytes} bytes
          </dd>
        </div>
        <div>
          <dt>Tokens</dt>
          <dd>
            {record.promptTokens} tokens in, {record.completionTokens} out
          </dd>
        </div>
      </dl>
      <p className="nc-cap" data-testid="cover">
        {text.describeGap(cover.gapSecs)} after the deposit · {cover.between} deposits joined the
        tree in between, {cover.externalBetween} external
      </p>
    </li>
  );
}

/** Operator text, shown as plain text and cut to this many characters. */
export const OPERATOR_TEXT_CAP = 300;
const plain = (text: string) => {
  const flat = text.replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ');
  return flat.length > OPERATOR_TEXT_CAP ? `${flat.slice(0, OPERATOR_TEXT_CAP)}…` : flat;
};

const STATUS_TEXT: Record<string, StringKey> = {
  finalized: 'reuse_finalized',
  reserved: 'reuse_reserved',
  clearance_reserved: 'reuse_clearance_reserved',
  unknown: 'reuse_unknown',
};

type Reuse = {
  status: string;
  /** What happened to the stored request: not sent (and why), refused, or accepted. */
  resent:
    | { kind: 'not-sent'; why: 'left' | 'old' | 'missing' }
    | { kind: 'refused'; code: string; says: string }
    | { kind: 'accepted' }
    | { kind: 'failed'; why: string };
};

function ReuseCheck({ note, view }: { note: NoteRecord; view: NoteView }) {
  const operator = useMemo(() => new OperatorClient(PROFILE.operatorUrl), []);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Reuse | { error: string } | null>(null);
  const last = view.last;

  const check = async () => {
    if (!last) return;
    setBusy(true);
    setResult(null);
    try {
      const status = await operator.nullifierStatus(last.nullifier);
      const left = view.exit !== undefined || note.leave !== undefined;
      const inside = Math.floor(Date.now() / 1000) < last.requestTime + REQUEST_LIFETIME_SECS;
      if (left) return setResult({ status, resent: { kind: 'not-sent', why: 'left' } });
      if (!inside) return setResult({ status, resent: { kind: 'not-sent', why: 'old' } });
      const answer = await operator.resend(last.json).catch((e: unknown) => e);
      if (answer instanceof OperatorError) {
        return setResult({
          status,
          resent: { kind: 'refused', code: answer.code, says: answer.message },
        });
      }
      if (answer instanceof OperatorUnreachable) {
        return setResult({ status, resent: { kind: 'failed', why: answer.message } });
      }
      if (answer instanceof Error) throw answer;
      setResult({ status, resent: { kind: 'accepted' } });
    } catch (e) {
      setResult({ error: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card title="Check reuse" aside={<Chip>asks the operator</Chip>} testId="reuse">
      {!last ? (
        <p className="nc-cap nc-cap-first" data-testid="reuse-none">
          {sentence('reuse_none')}
        </p>
      ) : (
        <>
          <p className="nc-cap nc-cap-first">{sentence('reuse_evidence')}</p>
          <div className="nc-actions">
            <Button disabled={busy} testId="reuse-check" onClick={check}>
              {busy ? 'Asking the operator…' : 'Check reuse'}
            </Button>
          </div>
        </>
      )}
      {result && 'error' in result && (
        <p className="nc-flag" data-testid="reuse-error">
          {fill('reuse_unreachable', { why: plain(result.error) })}
        </p>
      )}
      {result && 'status' in result && (
        <div className="nc-reuse" data-testid="reuse-result">
          <p
            className="nc-msg"
            data-tone={result.status === 'finalized' ? 'ok' : undefined}
            data-testid="reuse-status"
          >
            {result.status in STATUS_TEXT
              ? sentence(STATUS_TEXT[result.status]!)
              : fill('reuse_other', { status: plain(result.status) })}
          </p>
          {result.resent.kind === 'not-sent' && (
            <p data-testid="reuse-not-sent">
              {sentence(
                result.resent.why === 'left'
                  ? 'reuse_left'
                  : result.resent.why === 'old'
                    ? 'reuse_too_old'
                    : 'reuse_no_request',
              )}
            </p>
          )}
          {result.resent.kind === 'refused' && (
            <>
              <p>{sentence('reuse_resent')}</p>
              <p className="nc-flag" data-testid="reuse-refused">
                <strong>{sentence('reuse_refuses')}</strong>
                {result.resent.code ? ` (${plain(result.resent.code)})` : ''}
              </p>
              <div className="nc-says" data-testid="operator-says">
                <small>{sentence('reuse_says')}</small>
                <pre data-testid="operator-says-text">{plain(result.resent.says)}</pre>
              </div>
              <p className="nc-cap">{sentence('reuse_read_reason')}</p>
            </>
          )}
          {result.resent.kind === 'accepted' && (
            <>
              <p>{sentence('reuse_resent')}</p>
              <p className="nc-flag" data-testid="reuse-accepted">
                {sentence('reuse_accepted')}
              </p>
            </>
          )}
          {result.resent.kind === 'failed' && (
            <p className="nc-flag" data-testid="reuse-failed">
              {fill('reuse_unreachable', { why: plain(result.resent.why) })}
            </p>
          )}
        </div>
      )}
      <p className="nc-cap" data-testid="reuse-rule">
        {sentence('reuse_rule')}
      </p>
    </Card>
  );
}

const LEAVING: StringKey[] = [
  'leave_close',
  'leave_clearance',
  'leave_exit',
  'leave_answer',
  'leave_sender',
];

function Leaving() {
  return (
    <section className="nc-leave" data-testid="leaving">
      <h2>What leaving reveals</h2>
      <ul className="nc-leaving">
        {LEAVING.map((k) => (
          <li key={k}>
            <span className="nc-vmark" aria-hidden="true" />
            {sentence(k)}
          </li>
        ))}
      </ul>
      <p className="nc-callout" data-testid="exit-purpose">
        {sentence('leave_exit_purpose')}
      </p>
    </section>
  );
}

function Observers({ note }: { note: NoteRecord }) {
  const [open, setOpen] = useState<string | null>(null);
  const rows: [string, StringKey[]][] = [
    ['Chain', ['who_chain']],
    ['Operator', ['who_operator']],
    ["Operator's public entry", ['who_entry']],
    ["Operator's second machine", ['who_second']],
    ['NEAR AI', ['who_model', 'who_near_ai']],
    ['RPC providers', ['who_rpc']],
    ['Web host', ['who_host']],
    ['Attestation services', ['who_attest']],
    ['Certificate logs', ['who_certs']],
    ...(note.sender === INTENTS_SIGNER
      ? [['1Click and the Intents explorer', ['who_intents']] as [string, StringKey[]]]
      : []),
  ];
  return (
    <section className="nc-observers" data-testid="observers">
      <h2>Who sees what</h2>
      <div className="nc-matrix">
        {rows.map(([who, keys]) => (
          <div key={who} className="nc-matrix-row" data-open={open === who} data-testid="observer">
            <button
              type="button"
              aria-expanded={open === who}
              onClick={() => setOpen(open === who ? null : who)}
            >
              <span>{who}</span>
              <Icon name="chevron" size={12} />
            </button>
            <p hidden={open !== who}>{keys.map(sentence).join(' ')}</p>
          </div>
        ))}
      </div>
      <p className="nc-cap">Open a row for the sentence itself.</p>
    </section>
  );
}

const SECTIONS = [
  ['overview', 'Overview'],
  ['leases', 'Leases'],
  ['chain', 'On chain'],
  ['observers', 'Who sees what'],
  ['leaving', 'Leaving and reuse'],
  ['limits', 'Limits'],
] as const;

type Tab = (typeof SECTIONS)[number][0];

/** Opens the lease tab if needed, then brings one lease row into view. */
const showLease = (setTab: (t: Tab) => void, id: string) => {
  setTab('leases');
  window.setTimeout(
    () => document.getElementById(id)?.scrollIntoView({ behavior: 'smooth', block: 'center' }),
    60,
  );
};

function Panel({ note, view, onBack }: { note: NoteRecord; view: NoteView; onBack: () => void }) {
  const [now, setNow] = useState<Now>({ status: 'loading' });
  const [more, setMore] = useState(false);
  const [tab, setTab] = useState<Tab>('overview');
  const leases: LeaseItem[] = [
    ...view.leases.map((record) => ({ record, open: false })),
    ...(view.active ? [{ record: (view.active as ActiveLease).record, open: true }] : []),
  ].sort((a, b) => a.record.openedAt - b.record.openedAt);
  const lastFigure = [...leases]
    .reverse()
    .map((l) => l.record.setSize)
    .find(isFigure);

  useEffect(() => {
    let live = true;
    readNow(setSizeRpcs, PROFILE.vaultAccount, Math.floor(Date.now() / 1000)).then(
      (raw) => {
        if (live) {
          setNow({ status: 'ok', size: evaluate(raw, pinned(), lastFigure?.operatorTeam ?? null) });
        }
      },
      (e) => {
        if (live) setNow({ status: 'failed', why: e instanceof Error ? e.message : String(e) });
      },
    );
    return () => {
      live = false;
    };
    // The figure is read once per opening of the panel.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const fee = PROFILE.pins?.config.note_fee;
  // Once a close, a clearance or an exit has started, the leases are tied to the note ID.
  const left = note.leave !== undefined || view.exit !== undefined;
  const allFlags = leases.flatMap((l) => {
    const figure = isFigure(l.record.setSize) ? l.record.setSize : undefined;
    const team = teamOf(pinned(), figure?.operatorTeam ?? null);
    const cover = coverOf(
      note.noteId ?? 0,
      Math.floor((note.depositedAt ?? note.createdAt) / 1000),
      l.record,
      team,
    );
    return flagsOf(l.record, figure, thin(cover)).map((f) => ({ ...f, l }));
  });
  const last = leases[leases.length - 1];
  const daysLeft =
    note.expiry !== undefined ? Math.ceil((note.expiry - Date.now() / 1000) / 86_400) : 30;
  const mark: { status: NoteStatus; day: number; text?: string } = {
    status:
      note.leave?.state === 'closed' || note.leave?.state === 'exited'
        ? 'closed'
        : note.leave?.state === 'closing'
          ? 'closing'
          : left
            ? 'exiting'
            : daysLeft <= CLOSE_DAYS
              ? 'expiring'
              : view.active
                ? 'leased'
                : 'open',
    day: 30 - daysLeft,
  };
  const lastView = last ? isFigure(last.record.setSize) : false;
  const depositedAt = Math.floor((note.depositedAt ?? note.createdAt) / 1000);
  const nowSecs = Math.floor(Date.now() / 1000);
  const bars: SetBar[] = leases.flatMap((l) =>
    isFigure(l.record.setSize)
      ? [
          {
            key: slug(l.record.leaseId || l.record.openedAt),
            external: l.record.setSize.external,
            total: l.record.setSize.total,
            open: l.open,
            label: utc(l.record.openedAt).slice(5, 10),
          },
        ]
      : [],
  );
  const flagged = new Set(allFlags.map((f) => f.l.record.leaseId || f.l.record.openedAt));
  const stops: Stop[] = leases.map((l) => ({
    key: slug(l.record.leaseId || l.record.openedAt),
    at: l.record.openedAt,
    label: utc(l.record.openedAt),
    tone: flagged.has(l.record.leaseId || l.record.openedAt) ? 'warn' : l.open ? 'open' : undefined,
  }));
  const stages: Stage[] = [
    {
      key: 'deposit',
      label: 'Deposit',
      lane: 'on chain',
      state: 'done',
      detail: utc(depositedAt).slice(0, 10),
    },
    {
      key: 'set',
      label: 'In the set',
      lane: 'on chain',
      state: 'done',
      detail: now.status === 'ok' ? `${now.size.external} external` : 'counted by the vault',
    },
    {
      key: 'leases',
      label: 'Leases',
      lane: 'operator',
      state: view.active ? 'now' : leases.length > 0 ? 'done' : 'todo',
      detail: leases.length === 0 ? 'none yet' : `${leases.length} so far`,
    },
    {
      key: 'leave',
      label: 'Leaving',
      lane: 'on chain',
      state: left ? 'now' : 'todo',
      detail: left ? (note.leave?.state ?? 'exit requested') : 'not started',
    },
  ];
  const onKey = (e: React.KeyboardEvent) => {
    const i = SECTIONS.findIndex(([id]) => id === tab);
    const next =
      e.key === 'ArrowRight'
        ? (i + 1) % SECTIONS.length
        : e.key === 'ArrowLeft'
          ? (i + SECTIONS.length - 1) % SECTIONS.length
          : e.key === 'Home'
            ? 0
            : e.key === 'End'
              ? SECTIONS.length - 1
              : -1;
    if (next < 0) return;
    e.preventDefault();
    const id = SECTIONS[next]![0];
    setTab(id);
    document.getElementById(`ptab-${id}`)?.focus();
  };
  const panel = (id: Tab) =>
    ({
      id: `priv-${id}`,
      role: 'tabpanel',
      'aria-labelledby': `ptab-${id}`,
      hidden: tab !== id,
    }) as const;

  return (
    <div className="nc-privacy" data-testid="privacy-screen">
      <div className="nc-privacy-bar">
        <Button testId="privacy-back" onClick={onBack}>
          <Icon name="chevron" size={12} /> Back
        </Button>
        <h1>Privacy panel</h1>
        <Chip>note #{note.noteId}</Chip>
        <Status n={mark} />
      </div>
      <div className="nc-address" data-more={more}>
        <Icon name="onion" size={16} />
        <p data-testid="address-line">
          <strong>This page cannot hide your network address.</strong>{' '}
          <span hidden={more}>
            The operator, the hosts in front of it and the RPC providers see it.
          </span>
          <span hidden={!more}>{text.ADDRESS}</span>{' '}
          <button type="button" className="nc-link" onClick={() => setMore(!more)}>
            {more ? 'Less' : 'Read more'}
          </button>
        </p>
      </div>
      <div
        className="nc-ptabs"
        role="tablist"
        aria-label="Privacy panel sections"
        onKeyDown={onKey}
      >
        {SECTIONS.map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`ptab-${id}`}
            aria-selected={tab === id}
            aria-controls={`priv-${id}`}
            tabIndex={tab === id ? 0 : -1}
            onClick={() => setTab(id)}
          >
            {label}
            {id === 'leases' && <em>{leases.length}</em>}
            {id === 'overview' && allFlags.length > 0 && (
              <i className="nc-pdot" aria-label="has flags" />
            )}
          </button>
        ))}
      </div>

      <div className="nc-ppanel" {...panel('overview')}>
        <section className="nc-where">
          <h2>Where this note is</h2>
          <Journey stages={stages} />
        </section>
        <section className="nc-glance" data-testid="now-figure">
          <h2>At a glance</h2>
          <ul>
            <li className="nc-glance-main">
              <span className="nc-glance-k">Set size now</span>
              <span className="nc-glance-v">
                {now.status === 'loading' && (
                  <span className="nc-hold-inline">
                    <WaitBar /> Reading the vault…
                  </span>
                )}
                {now.status === 'failed' && (
                  <span data-testid="now-text">
                    {sentence('now_label')}: {fill('unavailable', { why: now.why })}
                  </span>
                )}
                {now.status === 'ok' && (
                  <>
                    <span className="nc-figure" aria-hidden="true">
                      {now.size.external}
                      <small>external</small>
                    </span>
                    <span data-testid="now-text">
                      {sentence('now_label')}: {headline(now.size)}
                    </span>
                    <SetCells
                      ext={now.size.external}
                      team={now.size.total - now.size.external}
                      label="Notes that could pay now"
                    />
                    <SetGauge external={now.size.external} total={now.size.total} />
                    <SetLegend />
                  </>
                )}
              </span>
              {now.status === 'ok' &&
                differences(now.size, lastView).map((d) => (
                  <small key={d} data-testid="now-difference">
                    {d}
                  </small>
                ))}
            </li>
            <li>
              <span className="nc-glance-k">Latest lease</span>
              <span className="nc-glance-v">
                {last ? utc(last.record.openedAt) : 'No lease has been made on this note yet.'}
              </span>
              <span className="nc-glance-k">Leases on this note</span>
              <span className="nc-glance-v">{leases.length}</span>
            </li>
          </ul>
        </section>

        <section className="nc-flags">
          <h2>
            What to watch <Chip tone={allFlags.length ? 'warn' : 'good'}>{allFlags.length}</Chip>
          </h2>
          {allFlags.length === 0 ? (
            <p className="nc-cap nc-cap-first">No lease on this note has a flag.</p>
          ) : (
            <ul>
              {allFlags.map((f) => (
                <li key={f.l.record.leaseId + f.kind}>
                  <Chip tone="warn">{FLAG_NAME[f.kind]}</Chip>
                  <span>{f.text}</span>
                  <button
                    type="button"
                    className="nc-link"
                    onClick={() =>
                      showLease(setTab, slug(f.l.record.leaseId || f.l.record.openedAt))
                    }
                  >
                    {utc(f.l.record.openedAt).slice(5, 16)}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="nc-join" data-testid="link-column">
          <h2>What joins the two views</h2>
          <div className="nc-join-diagram" aria-hidden="true">
            <span>
              <b>On chain</b>
              <small>deposit</small>
            </span>
            <i />
            <span>
              <b>Operator</b>
              <small>leases</small>
            </span>
          </div>
          <p data-testid="link-sentence" data-left={left}>
            {sentence(left ? 'link_left' : 'link_open')}
          </p>
        </section>
      </div>

      <section className="nc-ppanel" data-testid="operator-column" {...panel('leases')}>
        <h2>Leases</h2>
        <p className="nc-cap nc-cap-first">{text.RIGHT_INTRO}</p>
        {leases.length === 0 ? (
          <Waiting
            tone="none"
            flat
            testId="no-leases"
            title="No lease has been made on this note yet."
          />
        ) : (
          <>
            <Timeline
              start={depositedAt}
              end={Math.max(nowSecs, ...leases.map((l) => l.record.openedAt))}
              stops={stops}
              startLabel={`deposit ${utc(depositedAt).slice(0, 10)}`}
              endLabel="now"
              onPick={(k) => showLease(setTab, k)}
            />
            {bars.length > 0 && <SetBars bars={bars} onPick={(k) => showLease(setTab, k)} />}
            <ul className="nc-lease-pick" aria-label="Leases">
              {leases.map((l) => (
                <li key={l.record.leaseId || l.record.openedAt}>
                  <button
                    type="button"
                    onClick={() => showLease(setTab, slug(l.record.leaseId || l.record.openedAt))}
                  >
                    <span className="nc-lp-top">
                      <strong>{utc(l.record.openedAt).slice(5, 16)}</strong>
                      {l.open && <Chip tone="live">open</Chip>}
                    </span>
                    {isFigure(l.record.setSize) && (
                      <span className="nc-lp-set">
                        <b>{l.record.setSize.external}</b> could have paid
                      </span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
            <ul className="nc-leases">
              {leases.map((l) => (
                <LeaseRow key={l.record.leaseId || l.record.openedAt} {...l} note={note} />
              ))}
            </ul>
          </>
        )}
      </section>

      <section className="nc-onchain nc-ppanel" data-testid="chain-column" {...panel('chain')}>
        <h2>On chain</h2>
        <p className="nc-cap nc-cap-first">{text.LEFT_INTRO}</p>
        {note.expiry !== undefined && (
          <div className="nc-life" data-testid="note-life">
            <Timeline
              start={depositedAt}
              end={note.expiry}
              fill={nowSecs}
              stops={[{ key: 'today', at: nowSecs, label: 'today', tone: 'open' }]}
              startLabel={`deposit ${utc(depositedAt).slice(0, 10)}`}
              endLabel={`expiry ${formatDate(note.expiry)}`}
            />
          </div>
        )}
        <dl className="nc-facts">
          <div>
            <dt>Deposit transaction</dt>
            <dd>
              {note.txHash ? (
                <a
                  href={PROFILE.explorerTxUrl(note.txHash)}
                  target="_blank"
                  rel="noopener noreferrer"
                  data-testid="explorer-link"
                >
                  {note.txHash}
                </a>
              ) : (
                'no transaction hash is stored for this note'
              )}
            </dd>
          </div>
          <div>
            <dt>Note ID</dt>
            <dd data-testid="chain-note-id">{note.noteId}</dd>
          </div>
          <div>
            <dt>Commitment</dt>
            <dd>
              <code data-testid="chain-commitment">{note.commitment}</code>
            </dd>
          </div>
          <div>
            <dt>Denomination</dt>
            <dd data-testid="chain-denomination">{units(note.denomination)}</dd>
          </div>
          <div>
            <dt>Expiry</dt>
            <dd data-testid="chain-expiry">
              {note.expiry !== undefined ? formatDate(note.expiry) : 'unknown'}
            </dd>
          </div>
          <div>
            <dt>Payout account</dt>
            <dd data-testid="chain-payout">{note.payout}</dd>
          </div>
          <div>
            <dt>Note fee</dt>
            <dd data-testid="chain-fee">{fee !== undefined ? units(fee) : 'unknown'}</dd>
          </div>
        </dl>
      </section>

      <div className="nc-ppanel" {...panel('observers')}>
        <Observers note={note} />
      </div>

      <div className="nc-ppanel" {...panel('leaving')}>
        <Leaving />
        <ReuseCheck note={note} view={view} />
      </div>

      <div className="nc-ppanel" {...panel('limits')}>
        <section className="nc-limits" data-testid="honesty">
          <h2>What the figures do not say</h2>
          <ul className="nc-honest">
            <li>{sentence('balances_private')}</li>
            <li>{text.INCLUDES_OWN}</li>
            <li>{text.TEAM_DECLARATION}</li>
            <li>{sentence('includes_unlisted')}</li>
            <li>{sentence('five_not_safe')}</li>
            <li data-testid="thresholds">{text.THRESHOLDS}</li>
            {PROFILE.network === 'testnet' && (
              <li data-testid="testnet-text">{sentence('testnet_notes')}</li>
            )}
          </ul>
        </section>
        <Card
          title="The operator’s retention policy"
          aside={<Chip>the client cannot check it</Chip>}
          testId="policy"
        >
          <p className="nc-cap nc-cap-first">{text.POLICY_LABEL}</p>
          <p className="nc-policy-text">{text.POLICY}</p>
        </Card>
      </div>
    </div>
  );
}

export function Privacy({ noteId, onBack }: { noteId: string; onBack: () => void }) {
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const [loaded, setLoaded] = useState<{ note: NoteRecord; view: NoteView } | 'none' | null>(null);

  useEffect(() => {
    if (!unlocked) return;
    let live = true;
    (async () => {
      const note = (await loadNotes()).find((n) => n.id === noteId);
      if (!note || note.kind !== 'state') return live && setLoaded('none');
      const view = await callWorker({
        op: 'readNote',
        key: requireKey(),
        aad: noteAad(note.network, note.vault, note.id),
        sealed: note.sealed,
      });
      if (live) setLoaded({ note, view });
    })().catch(() => live && setLoaded('none'));
    return () => {
      live = false;
    };
  }, [unlocked, noteId]);

  if (!unlocked) return <Unlock onReady={() => setUnlocked(true)} />;
  if (loaded && loaded !== 'none') {
    return <Panel note={loaded.note} view={loaded.view} onBack={onBack} />;
  }
  return (
    <div className="nc-privacy" data-testid="privacy-screen">
      <div className="nc-privacy-bar">
        <Button testId="privacy-back" onClick={onBack}>
          <Icon name="chevron" size={12} /> Back
        </Button>
        <h1>Privacy panel</h1>
      </div>
      {loaded === 'none' ? (
        <Waiting tone="none" title="This note is not in this browser">
          It may have been removed, or it belongs to another vault. Go back and pick a note from the
          list.
        </Waiting>
      ) : (
        <Waiting title="Reading this note from the encrypted store" />
      )}
    </div>
  );
}
