// The privacy panel: what anyone can read on chain about a note (left) beside what the operator
// saw for each lease on it (right), with the set size of every lease. The left column comes from
// the deposit record this browser keeps and makes no request. The set size of a lease was counted
// when the lease opened and is stored with it; the "now" figure reads the vault's views again.
import { useEffect, useState } from 'react';
import { DEFAULT_BOUND } from '../chat/models';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { loadNotes } from '../funding/flow';
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
import { fill, sentence } from '../privacy/strings';
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

function LeaseRow({
  record,
  note,
  open,
}: {
  record: LeaseRecord;
  note: NoteRecord;
  open: boolean;
}) {
  const figure = isFigure(record.setSize) ? record.setSize : undefined;
  const team = teamOf(pinned(), figure?.operatorTeam ?? null);
  const cover = coverOf(
    note.noteId ?? 0,
    Math.floor((note.depositedAt ?? note.createdAt) / 1000),
    record,
    team,
  );
  const standsOut = BigInt(record.boundClass) > DEFAULT_BOUND;
  const s = record.sampling;
  const sampling = [
    s.temperature !== undefined ? `temperature ${s.temperature}` : null,
    s.top_p !== undefined ? `top_p ${s.top_p}` : null,
    s.stream ? 'streamed' : 'not streamed',
  ]
    .filter(Boolean)
    .join(', ');
  return (
    <li data-testid="lease-row" data-lease={record.leaseId}>
      <p>
        <strong data-testid="lease-time">{utc(record.openedAt)}</strong>
        {open && <em> (still open)</em>} · lease{' '}
        <code data-testid="lease-id">{record.leaseId}</code>
      </p>
      <p>
        Bound class <span data-testid="lease-bound">{units(record.boundClass)}</span> · charge{' '}
        <span data-testid="lease-charge">
          {record.charge !== undefined ? units(record.charge) : 'not settled yet'}
        </span>
      </p>
      {standsOut && (
        <p className="warning" data-testid="stands-out">
          {text.STANDS_OUT(units(record.boundClass))}
        </p>
      )}
      <p>
        Model {record.model || 'none yet'} · {sampling} · {record.messageCount} messages,{' '}
        {record.messageBytes} bytes · {record.promptTokens} tokens in, {record.completionTokens} out
      </p>
      {figure ? (
        <>
          <p data-testid="set-size">{headline(figure)}</p>
          {differences(figure).map((d) => (
            <p key={d} className="hint" data-testid="set-difference">
              {d}
            </p>
          ))}
          {small(figure.external) ? (
            <p className="warning" data-testid="small-set">
              {text.SMALL_SET}
            </p>
          ) : (
            <p className="hint" data-testid="five-not-safe">
              {sentence('five_not_safe')}
            </p>
          )}
        </>
      ) : (
        <p data-testid="set-size-missing">
          {record.setSize && 'unavailable' in record.setSize
            ? fill('unavailable', { why: record.setSize.unavailable })
            : text.NO_RECORD}
        </p>
      )}
      <p data-testid="cover">
        {text.describeGap(cover.gapSecs)} after the deposit · {cover.between} deposits joined the
        tree in between, {cover.externalBetween} external
      </p>
      {thin(cover) && (
        <p className="warning" data-testid="timing-warning">
          {text.TIMING_WARNING}
        </p>
      )}
    </li>
  );
}

function Panel({ note, view }: { note: NoteRecord; view: NoteView }) {
  const [now, setNow] = useState<Now>({ status: 'loading' });
  const leases: { record: LeaseRecord; open: boolean }[] = [
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
  return (
    <>
      <p className="notice" data-testid="address-line">
        {text.ADDRESS}
      </p>
      <div className="columns">
        <section data-testid="chain-column">
          <h3>On chain</h3>
          <p className="hint">{text.LEFT_INTRO}</p>
          <dl>
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
            <dt>Note ID</dt>
            <dd data-testid="chain-note-id">{note.noteId}</dd>
            <dt>Commitment</dt>
            <dd>
              <code data-testid="chain-commitment">{note.commitment}</code>
            </dd>
            <dt>Denomination</dt>
            <dd data-testid="chain-denomination">{units(note.denomination)}</dd>
            <dt>Expiry</dt>
            <dd data-testid="chain-expiry">
              {note.expiry !== undefined ? formatDate(note.expiry) : 'unknown'}
            </dd>
            <dt>Payout account</dt>
            <dd data-testid="chain-payout">{note.payout}</dd>
            <dt>Note fee</dt>
            <dd data-testid="chain-fee">{fee !== undefined ? units(fee) : 'unknown'}</dd>
          </dl>
        </section>
        <section data-testid="operator-column">
          <h3>What the operator saw</h3>
          <p className="hint">{text.RIGHT_INTRO}</p>
          {leases.length === 0 ? (
            <p data-testid="no-leases">No lease has been made on this note yet.</p>
          ) : (
            <ul className="leases">
              {leases.map((l) => (
                <LeaseRow key={l.record.leaseId || l.record.openedAt} {...l} note={note} />
              ))}
            </ul>
          )}
          <div data-testid="policy">
            <h4>{text.POLICY_LABEL}</h4>
            <p>{text.POLICY}</p>
          </div>
        </section>
      </div>
      <section data-testid="now-figure">
        <h3>Now</h3>
        {now.status === 'loading' && <p>Reading the vault…</p>}
        {now.status === 'failed' && (
          <p data-testid="now-text">
            {sentence('now_label')}: {fill('unavailable', { why: now.why })}
          </p>
        )}
        {now.status === 'ok' && (
          <>
            <p data-testid="now-text">
              {sentence('now_label')}: {headline(now.size)}
            </p>
            {differences(now.size, lastFigure !== undefined).map((d) => (
              <p key={d} className="hint" data-testid="now-difference">
                {d}
              </p>
            ))}
          </>
        )}
      </section>
      <section data-testid="honesty">
        <h3>What the figures do not say</h3>
        <ul>
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
    </>
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
  return (
    <section data-testid="privacy-screen">
      <h2>Privacy panel</h2>
      <button type="button" data-testid="privacy-back" onClick={onBack}>
        Back
      </button>
      {loaded === null && <p>Loading…</p>}
      {loaded === 'none' && <p>This note is not in this browser.</p>}
      {loaded && loaded !== 'none' && <Panel note={loaded.note} view={loaded.view} />}
    </section>
  );
}
