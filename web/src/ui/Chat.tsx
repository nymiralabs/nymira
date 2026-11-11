import { Fragment, useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import './chat-idle.css';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked, requireKey, typedUnlockPassphrase } from '../crypto/wrap';
import { SameAsUnlock } from '../marketing/kit';
import { loadNotes, makeBackupFile } from '../funding/flow';
import { noteAad, type NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { backupDue } from '../chat/backup-policy';
import { retireBackup } from '../chat/retire-backup';
import { DEFAULT_BOUND, DEFAULT_MODEL, LONG_JOB_BOUND, MODELS } from '../chat/models';
import { Count } from '../marketing/Board';
import { Icon } from '../marketing/Icon';
import {
  Button,
  CHECK_ROWS,
  Chip,
  END_ROWS,
  Facts,
  Fold,
  InspectorShell,
  ModelPicker,
  Seal,
  Status,
  Strip,
  WaitBar,
  Waiting,
  VerifyList,
  clamp,
  modelInfo,
  type NoteStatus,
} from '../marketing/kit';
import { chatDeps, proverNow, warmProver, type ProverState } from '../chat/runtime';
import { ChatError, ChatSession, type Answer, type Message, type Settled } from '../chat/session';
import { badgeLabel } from '../chat/attest/signature';
import type { NoteView } from '../note/lease';
import { download, formatDate, formatUnits } from './format';
import { CLOSE_DAYS, leaseBlocked } from '../leave/guard';
import { leaveStore } from '../leave/store';
import { Unlock } from './Unlock';
import { ChatDemo, SUGGESTIONS } from './ChatDemo';
import type { VaultCheck } from '../chain/vault';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;

interface FsHandle {
  createWritable(): Promise<{ write(data: BlobPart): Promise<void>; close(): Promise<void> }>;
}
type PickerWindow = Window & {
  showSaveFilePicker?: (o: unknown) => Promise<FsHandle>;
};

const MODEL_INFOS = MODELS.map((m) => modelInfo(m.id, Number(m.inputNano), Number(m.outputNano)));

const nowSecs = () => Math.floor(Date.now() / 1000);
const fsAccess = () => typeof (window as PickerWindow).showSaveFilePicker === 'function';

function Badge({ settled }: { settled: Settled }) {
  const t = settled.trust;
  return (
    <div data-testid="badge" data-kind={t.result}>
      <h3>
        <Chip tone={t.result === 'not_verified' ? 'warn' : 'good'}>
          <span data-testid="badge-text">{badgeLabel(t)}</span>
        </Chip>
        {t.result === 'not_verified' && <> ({t.reason})</>}
      </h3>
      <p data-testid="encryption-line">
        Encryption: your messages were end-to-end encrypted to the model’s attested key; the
        operator saw the model, settings, message count and size, usage and timing.
      </p>
      <p data-testid="tls-note">
        This browser cannot bind the gateway’s TLS certificate, so a gateway signature shows who
        signed, not which server you reached.
      </p>
    </div>
  );
}

export function Chat({
  vault,
  onPrivacy,
  onFund,
  onLeaving,
  onActive,
  preferred,
}: {
  vault: VaultCheck | null;
  onPrivacy: (noteId: string) => void;
  onFund: () => void;
  /** Opens the Close screen: a note on its way out is followed there. */
  onLeaving: (recordId?: string) => void;
  /** The note the conversation uses, or null, for the note list in the sidebar. */
  onActive: (recordId: string | null) => void;
  /** The note picked in the sidebar: the conversation opens on it without a list to choose from. */
  preferred: string | null;
}) {
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const [notes, setNotes] = useState<{ record: NoteRecord; view: NoteView }[]>([]);
  const [prover, setProver] = useState<ProverState>(proverNow() ?? { status: 'loading' });
  const [showAll, setShowAll] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [model, setModel] = useState(DEFAULT_MODEL);
  const [idleDraft, setIdleDraft] = useState('');
  const [pickFilter, setPickFilter] = useState<'all' | 'ready' | 'attention'>('all');
  const [pickQuery, setPickQuery] = useState('');
  const [demo, setDemo] = useState<(typeof SUGGESTIONS)[number] | null>(null);
  const [longJob, setLongJob] = useState(false);
  const [stream, setStream] = useState(false);
  const [input, setInput] = useState('');
  const [messages, setMessages] = useState<Message[]>([]);
  const [answers, setAnswers] = useState<Answer[]>([]);
  const [busy, setBusy] = useState('');
  const [busyKind, setBusyKind] = useState<'send' | 'end'>('send');
  const [error, setError] = useState('');
  const [settled, setSettled] = useState<Settled | null>(null);
  const [attestation, setAttestation] = useState('');
  const backup = useSyncExternalStore(retireBackup.subscribe, () => retireBackup.state);
  const [passphrase, setPassphrase] = useState('');
  const [fileHandle, setFileHandle] = useState<FsHandle | null>(null);
  const session = useRef<ChatSession | null>(null);
  const threadEnd = useRef<HTMLDivElement>(null);
  const leave = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);

  useEffect(() => {
    warmProver().then(setProver);
  }, []);

  // Picking another note in the sidebar goes back to opening it on its own.
  useEffect(() => setShowAll(false), [preferred]);

  // The thread is the one part of the screen that scrolls: it follows the conversation down.
  useEffect(() => {
    threadEnd.current?.scrollIntoView({ block: 'end', behavior: 'smooth' });
  }, [messages.length, busy, settled, error, selected]);

  useEffect(() => {
    onActive(selected);
  }, [selected, onActive]);

  useEffect(() => {
    if (unlocked) void leaveStore.checkEmergency(true);
  }, [unlocked]);

  const refresh = useCallback(async () => {
    if (!isUnlocked()) return;
    const all = (await loadNotes()).filter(
      (n) => n.kind === 'state' && n.phase === 'accepted' && !n.leave,
    );
    const key = requireKey();
    const withView = await Promise.all(
      all.map(async (record) => ({
        record,
        view: await callWorker({
          op: 'readNote',
          key,
          aad: noteAad(record.network, record.vault, record.id),
          sealed: record.sealed,
        }),
      })),
    );
    setNotes(withView);
  }, []);

  useEffect(() => {
    if (unlocked) void refresh();
  }, [unlocked, refresh]);

  // Closing the tab cannot wait for the settlement, but it can end the lease.
  useEffect(() => {
    const hide = () => session.current?.retireOnPageHide();
    window.addEventListener('pagehide', hide);
    return () => window.removeEventListener('pagehide', hide);
  }, []);

  // Leaving the conversation retires the lease, so no slot fee is paid for idle time.
  useEffect(
    () => () => {
      const s = session.current;
      if (s?.lease?.token)
        void s
          .end()
          .then(() => retireBackup.set({ needed: true, leases: 0 }))
          .catch(() => undefined);
      else void s?.abandon();
    },
    [],
  );

  const fail = (e: unknown) =>
    setError(e instanceof ChatError ? e.message : e instanceof Error ? e.message : String(e));

  const start = async (record: NoteRecord) => {
    setError('');
    setSettled(null);
    setMessages([]);
    setAnswers([]);
    try {
      const deps = await chatDeps(requireKey);
      session.current = new ChatSession(deps, record, longJob);
      // The first proof is made while the user types.
      session.current.prepareAhead();
      setSelected(record.id);
    } catch (e) {
      fail(e);
    }
  };

  // With a note picked (the first one by default) there is nothing to choose: the conversation
  // opens on it. The list stays for what needs a decision: an open lease to settle, a note too
  // close to its expiry, an emergency notice.
  useEffect(() => {
    if (selected || busy || settled || error || !unlocked) return;
    if (prover.status !== 'ready' || leave.emergency !== null) return;
    const target = notes.find((n) => n.record.id === preferred) ?? notes[0];
    if (!target || target.view.pending) return;
    if (leave.settings.enabled && leaseBlocked(target.view.expiry, nowSecs())) return;
    void start(target.record);
    // `start` only reads state that the guards above already cover.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [notes, preferred, selected, busy, settled, error, unlocked, prover.status, leave.emergency]);

  // A note picked in the sidebar while a conversation is open: with no lease open nothing is lost,
  // so the conversation moves to that note. With a lease open it stays, and says why.
  useEffect(() => {
    if (!selected || !preferred || preferred === selected || busy) return;
    // A note that is closing or closed is not in `notes`: the conversation stays where it is, or
    // it would drop the note it has, fall back to the first one and flip again.
    if (!notes.some((n) => n.record.id === preferred)) return;
    if (session.current?.lease?.token) {
      setError('This conversation has a lease open. End it before you switch to another note.');
      return;
    }
    void session.current?.abandon();
    session.current = null;
    setError('');
    setSettled(null);
    setMessages([]);
    setAnswers([]);
    setSelected(null);
  }, [preferred, selected, busy, notes]);

  // Long job changed with no lease open: the proof made ahead was for the old class, so it is set
  // aside and a new one starts on the same note.
  useEffect(() => {
    const s = session.current;
    if (!selected || !s || busy || s.lease?.token || s.longJob === longJob) return;
    void s.abandon();
    void start(s.record);
    // `start` only reads state that the guards above already cover.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [longJob, selected, busy]);

  if (!unlocked) return <Unlock onReady={() => setUnlocked(true)} />;

  if (prover.status === 'off') {
    return (
      <section data-testid="chat-off" className="nc-screen">
        <div className="nc-screen-main">
          <h1>Chat</h1>
          <p className="nc-notice" role="alert">
            {prover.reason} Chat is off in this build.
          </p>
        </div>
      </section>
    );
  }

  const send = async () => {
    const s = session.current;
    if (!s || !input.trim()) return;
    setError('');
    const next: Message[] = [...messages, { role: 'user', content: input }];
    setMessages(next);
    setInput('');
    try {
      setBusyKind('send');
      setBusy('Checking the model’s attestation, then sending…');
      const answer = await s.send(next, { model, stream });
      setMessages([...next, { role: 'assistant', content: answer.text }]);
      setAnswers(s.answers.slice());
      if (s.attestation) {
        const a = s.attestation;
        setAttestation(
          `Attested: Intel TDX quote verified here (TCB ${a.model.tcbStatus}), deployment in the signed policy. NVIDIA GPU evidence: ${
            a.model.gpu === 'nonce_only'
              ? 'its nonce matched; the evidence itself is not verified in a browser'
              : 'not checked'
          }.`,
        );
      }
    } catch (e) {
      setMessages(messages);
      fail(e);
    } finally {
      setBusy('');
    }
  };

  const afterRetire = async (result: Settled) => {
    await refresh();
    // Closing at the end of a session is off by default: every close shows the note ID and the
    // total spend on chain. When the user switched it on, the close starts here and waits for the
    // wallet's signature.
    if (leave.settings.closeAtSessionEnd && session.current) {
      void leaveStore.close(session.current.record, 'wallet');
    }
    const leases = (await session.current?.view())?.leases.length ?? 0;
    const balance = BigInt(result.balance);
    const needed = backupDue(balance, leases);
    if (fileHandle && passphrase) {
      const bytes = await makeBackupFile(passphrase);
      const w = await fileHandle.createWritable();
      await w.write(bytes as BlobPart);
      await w.close();
      retireBackup.set({ needed: false, leases });
    } else {
      retireBackup.set({ needed, leases });
    }
  };

  const end = async () => {
    const s = session.current;
    if (!s) return;
    setError('');
    setBusyKind('end');
    setBusy('Ending the lease and checking the signatures…');
    try {
      const result = await s.end({ pollMs: 500 });
      setAnswers(s.answers.slice());
      setSettled(result);
      await afterRetire(result);
    } catch (e) {
      fail(e);
    } finally {
      setBusy('');
    }
  };

  const downloadBackup = async () => {
    try {
      const bytes = await makeBackupFile(passphrase);
      download(bytes as BlobPart, backupFileName(new Date()), 'application/octet-stream');
      retireBackup.clear();
    } catch (e) {
      fail(e);
    }
  };

  const chooseFile = async () => {
    try {
      const handle = await (window as PickerWindow).showSaveFilePicker!({
        suggestedName: backupFileName(new Date()),
      });
      setFileHandle(handle);
    } catch {
      // The picker was closed: nothing changes.
    }
  };

  const settleOpen = async (record: NoteRecord) => {
    setError('');
    setBusyKind('end');
    setBusy('Settling the lease this note has open…');
    try {
      const deps = await chatDeps(requireKey);
      const s = new ChatSession(deps, record, longJob);
      session.current = s;
      const result = await s.resume({ pollMs: 500 });
      if (result === 'set_aside') {
        setError(
          'The operator never received that request, so it was set aside. The note is free again.',
        );
        session.current = null;
      } else {
        setSelected(record.id);
        setSettled(result);
        await afterRetire(result);
      }
      await refresh();
    } catch (e) {
      fail(e);
    } finally {
      setBusy('');
    }
  };

  const open = selected ? session.current : null;
  const leaseOpen = Boolean(open?.lease?.token);
  const pass12 = passphraseLength(passphrase) >= MIN_PASSPHRASE_LENGTH;
  const boundBase = longJob ? LONG_JOB_BOUND : DEFAULT_BOUND;
  const bound = units(boundBase);
  const current = selected ? notes.find((n) => n.record.id === selected) : undefined;
  const daysLeft = current ? Math.ceil((current.view.expiry - nowSecs()) / 86_400) : 30;
  const noteMark: { status: NoteStatus; day: number } = {
    status: leaseOpen ? 'leased' : daysLeft <= CLOSE_DAYS ? 'expiring' : 'open',
    day: 30 - daysLeft,
  };
  // The share of the deposit still on the note, 0 to 1, for the cells.
  const shareOf = (n?: { record: NoteRecord; view: NoteView }) => {
    if (!n) return 0;
    try {
      return clamp(Number(BigInt(n.view.balance)) / Number(BigInt(n.record.denomination)));
    } catch {
      return 0;
    }
  };
  const decimals = 10 ** PROFILE.tokenDecimals;
  const used = answers.reduce((t, a) => t + a.usageMicro, 0);
  const boundNum = Number(boundBase);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !busy) {
      e.preventDefault();
      void send();
    }
  };
  // No funded note yet: the whole chat is shown, switched off, with a way to Fund.
  const idle = !selected && notes.length === 0;
  // A note was picked in the sidebar and only the proving key is missing: the conversation opens
  // on it by itself, so the screen says so instead of showing the list for a moment.
  const opening =
    !selected && !idle && !showAll && prover.status === 'loading' && preferred !== null
      ? notes.find(
          (n) =>
            n.record.id === preferred &&
            !n.view.pending &&
            leave.emergency === null &&
            !(leave.settings.enabled && leaseBlocked(n.view.expiry, nowSecs())),
        )
      : undefined;
  const picker = (
    <ModelPicker models={MODEL_INFOS} value={model} onChange={setModel} testId="model-select" />
  );
  const demoView = demo ? <ChatDemo question={demo} model={model} onAsk={setDemo} /> : null;
  const seal = busy && (
    <Seal
      title={busy}
      titleTestId="chat-busy"
      badge={<Chip tone="live">working</Chip>}
      tag={busyKind === 'send' ? 'X25519 · XChaCha20-Poly1305' : 'signed usage'}
      rows={busyKind === 'send' ? CHECK_ROWS : END_ROWS}
      done={0}
      auto
    />
  );

  // The note picker: the notes that can start a chat first (the fullest one suggested), then the ones
  // with a lease to settle, then the ones too close to their expiry.
  const pickRows = notes
    .map(({ record, view }) => {
      const near = leave.settings.enabled && leaseBlocked(view.expiry, nowSecs());
      const days = Math.max(0, Math.ceil((view.expiry - nowSecs()) / 86_400));
      const rank = view.pending ? 1 : near ? 2 : 0;
      return { record, view, near, days, rank };
    })
    .sort((a, b) =>
      a.rank !== b.rank
        ? a.rank - b.rank
        : a.rank === 2
          ? a.days - b.days
          : BigInt(b.view.balance) > BigInt(a.view.balance)
            ? 1
            : BigInt(b.view.balance) < BigInt(a.view.balance)
              ? -1
              : 0,
    );
  const bestId = pickRows.length >= 3 && pickRows[0]?.rank === 0 ? pickRows[0].record.id : null;
  const pickShown = pickRows.filter(
    (r) =>
      (pickFilter === 'all' || (pickFilter === 'ready') === (r.rank === 0)) &&
      (pickQuery.trim() === '' ||
        String(r.view.noteId).includes(pickQuery.trim().replace('#', ''))),
  );
  const pickReady = pickRows.filter((r) => r.rank === 0).length;

  return (
    <section
      data-testid="chat-screen"
      className={`nc-screen nc-screen-side nc-chat-screen${idle ? ' nc-chat-idle' : ''}`}
    >
      <div className="nc-chat-main">
        <h1 className="nc-sr">Chat</h1>
        {(selected || idle) && (
          <div className="nc-chat-head">
            {picker}
            {idle && (
              <div className="nc-chat-head-end">
                <Chip>Example · sample data</Chip>
              </div>
            )}
            {selected && !settled && (
              <div className="nc-chat-head-end nc-lease-bar" data-open={leaseOpen}>
                <span className="nc-state" data-s={leaseOpen ? 'leased' : 'open'}>
                  <i aria-hidden="true" />
                  {leaseOpen ? <>Lease open · {bound} reserved</> : 'No lease yet'}
                </span>
                <Button testId="chat-end" disabled={Boolean(busy) || !leaseOpen} onClick={end}>
                  End conversation
                </Button>
              </div>
            )}
          </div>
        )}
        <div className="nc-thread" aria-live="polite">
          {vault?.status === 'mismatch' && (
            <p className="nc-notice" role="alert">
              The vault differs from what this build expects, so no new lease will open.
            </p>
          )}
          {leave.emergency && !selected && (
            <p className="nc-notice" role="alert" data-testid="chat-emergency">
              The operator posted an emergency notice. No new lease is opened. Use the Close screen
              to request an exit.
            </p>
          )}
          {idle && (
            <div data-testid="chat-notes" style={{ display: 'contents' }}>
              {demoView ?? (
                <div className="nc-empty">
                  <h2>Ask an attested model.</h2>
                  <p>
                    Your prompt is encrypted to the model’s key before it leaves this page. The
                    first message opens a lease that reserves {bound}.
                  </p>
                  <div className="nc-chat-fund">
                    <p>Need a new note? Connect a wallet and fund one first.</p>
                    <div>
                      <Button onClick={onFund}>Fund a note</Button>
                    </div>
                  </div>
                  <div className="nc-suggest">
                    {SUGGESTIONS.map((q) => (
                      <button key={q} type="button" onClick={() => setDemo(q)}>
                        {q}
                        <Icon name="right" size={14} />
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}
          {opening && (
            <div data-testid="chat-opening" className="nc-opening">
              <Waiting title={`Opening note #${opening.view.noteId}`}>
                Loading the proving key (SHA-256 checked). The conversation opens on this note as
                soon as it is ready. Nothing is sent yet.
              </Waiting>
              <dl className="nc-opening-facts" data-testid="chat-opening-facts">
                <div>
                  <dt>Balance</dt>
                  <dd>
                    {units(opening.view.balance)} of {units(opening.record.denomination)}
                    <Strip n={10} solid={shareOf(opening) * 10} size="sm" />
                  </dd>
                </div>
                <div>
                  <dt>Expires</dt>
                  <dd>
                    {(() => {
                      const days = Math.max(
                        0,
                        Math.ceil((opening.view.expiry - nowSecs()) / 86_400),
                      );
                      return days === 1 ? 'in 1 day' : `in ${days} days`;
                    })()}
                  </dd>
                </div>
              </dl>
              <div className="nc-actions">
                <Button testId="chat-show-all" onClick={() => setShowAll(true)}>
                  Show all notes
                </Button>
              </div>
            </div>
          )}
          {!selected && !idle && !opening && (
            <>
              <div className="nc-empty">
                <h2>Ask an attested model.</h2>
                <p>
                  {notes.length > 1 ? 'Choose a note to chat with. ' : ''}Your prompt is encrypted
                  to the model’s key before it leaves this page. The first message opens a lease
                  that reserves {bound}.
                </p>
              </div>
              <div data-testid="chat-notes" className="nc-pick">
                {prover.status === 'loading' && (
                  <Waiting title="Please wait a moment" testId="chat-blocked">
                    Loading the proving key (SHA-256 checked). Chat opens as soon as it is ready.
                  </Waiting>
                )}
                {pickRows.length >= 5 && (
                  <div className="nc-pick-bar" data-testid="pick-filter">
                    <div role="group" aria-label="Filter notes">
                      {(
                        [
                          ['all', `All ${pickRows.length}`],
                          ['ready', `Ready ${pickReady}`],
                          ['attention', `Needs attention ${pickRows.length - pickReady}`],
                        ] as const
                      ).map(([id, label]) => (
                        <button
                          key={id}
                          type="button"
                          aria-pressed={pickFilter === id}
                          onClick={() => setPickFilter(id)}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                    <input
                      type="search"
                      value={pickQuery}
                      placeholder="Find #"
                      aria-label="Find a note by its number"
                      onChange={(e) => setPickQuery(e.target.value)}
                    />
                  </div>
                )}
                {pickShown.length === 0 && (
                  <p className="nc-cap" data-testid="pick-empty">
                    No note matches.{' '}
                    <button
                      type="button"
                      className="nc-link"
                      onClick={() => {
                        setPickFilter('all');
                        setPickQuery('');
                      }}
                    >
                      Show all
                    </button>
                  </p>
                )}
                <ul className="nc-pick-list" data-many={pickRows.length >= 5}>
                  {pickShown.map(({ record, view, near, days, rank }, idx) => {
                    const why =
                      prover.status === 'failed'
                        ? `Chat is off: the proving key was refused (${prover.reason}).`
                        : leave.emergency !== null
                          ? 'The operator’s emergency notice stops new leases.'
                          : null;
                    const first = idx === 0 || pickShown[idx - 1]!.rank !== rank;
                    const mixed = pickShown.some((r) => r.rank !== pickShown[0]!.rank);
                    return (
                      <Fragment key={record.id}>
                        {first && mixed && (
                          <li className="nc-pick-head" aria-hidden="true">
                            {rank === 0 ? 'Ready to chat' : 'Needs attention'}
                          </li>
                        )}
                        <li
                          data-testid="chat-note"
                          className="nc-pick-row"
                          data-rank={rank}
                          data-best={record.id === bestId || undefined}
                        >
                          <span className="nc-pick-id">
                            <strong>#{view.noteId}</strong>
                            <small>{units(view.balance)}</small>
                          </span>
                          <span className="nc-pick-life">
                            <Strip
                              n={10}
                              solid={shareOf({ record, view }) * 10}
                              size="sm"
                              label={`${units(view.balance)} of ${units(record.denomination)} left`}
                            />
                            <small
                              className="nc-state"
                              data-s={view.pending ? 'leased' : near ? 'expiring' : 'open'}
                            >
                              <i aria-hidden="true" />
                              {view.pending ? (
                                'lease open'
                              ) : near ? (
                                <>
                                  <span>
                                    expires in {days} {days === 1 ? 'day' : 'days'}
                                  </span>
                                  <span className="nc-pick-near" data-testid="note-near-expiry">
                                    no new lease: close it or request an exit
                                  </span>
                                </>
                              ) : (
                                <>
                                  <span>open</span>
                                  <span>expires {formatDate(view.expiry)}</span>
                                </>
                              )}
                              {view.pending && (
                                <Chip tone="warn">
                                  <span data-testid="note-pending">(a lease is not settled)</span>
                                </Chip>
                              )}
                              {record.id === bestId && <em className="nc-pick-best">suggested</em>}
                            </small>
                          </span>
                          <span className="nc-pick-act">
                            {view.pending ? (
                              <Button
                                primary
                                testId="chat-settle"
                                onClick={() => settleOpen(record)}
                              >
                                Settle the open lease
                              </Button>
                            ) : (
                              <>
                                {near && (
                                  <Button
                                    primary
                                    testId="chat-close-note"
                                    onClick={() => onLeaving(record.id)}
                                  >
                                    Close this note
                                  </Button>
                                )}
                                <Button
                                  primary={!near}
                                  testId="chat-use-note"
                                  disabled={
                                    prover.status !== 'ready' || leave.emergency !== null || near
                                  }
                                  onClick={() => start(record)}
                                >
                                  {prover.status === 'loading'
                                    ? 'Please wait…'
                                    : 'Chat with this note'}
                                </Button>
                              </>
                            )}
                            <Button testId="chat-privacy" onClick={() => onPrivacy(record.id)}>
                              Privacy panel
                            </Button>
                          </span>
                          {!near && why && (
                            <small
                              className="nc-pick-why"
                              data-tone={
                                prover.status === 'failed'
                                  ? 'err'
                                  : prover.status === 'loading'
                                    ? 'wait'
                                    : 'warn'
                              }
                              data-testid="chat-blocked"
                            >
                              {prover.status === 'loading' && (
                                <i className="nc-wait" aria-hidden="true">
                                  <b />
                                  <b />
                                  <b />
                                  <b />
                                  <b />
                                  <b />
                                </i>
                              )}
                              {why}
                            </small>
                          )}
                        </li>
                      </Fragment>
                    );
                  })}
                </ul>
                <label className="nc-keep">
                  <input
                    type="checkbox"
                    data-testid="long-job"
                    checked={longJob}
                    onChange={(e) => setLongJob(e.target.checked)}
                  />
                  <span className="nc-keep-switch" aria-hidden="true" />
                  <span className="nc-keep-text">
                    <strong>Long job</strong>
                    <small>
                      0.50 USDC lease class; fewer notes share it, so it is easier to tell apart
                    </small>
                  </span>
                </label>
              </div>
              {seal}
            </>
          )}
          {selected && (
            <>
              {messages.length === 0 &&
                !busy &&
                !settled &&
                (demoView ?? (
                  <div className="nc-empty">
                    <h2>Ask an attested model.</h2>
                    <p>
                      Your prompt is encrypted to its key before it leaves this page. The first
                      message opens a lease that reserves {bound}. The proof for it is made while
                      you type.
                    </p>
                    <div className="nc-suggest">
                      {SUGGESTIONS.map((q) => (
                        <button key={q} type="button" onClick={() => setDemo(q)}>
                          {q}
                          <Icon name="right" size={14} />
                        </button>
                      ))}
                    </div>
                  </div>
                ))}
              {leaseOpen && (
                <p className="nc-divider">
                  <span>Lease opened · reserves {bound}</span>
                </p>
              )}
              <ol
                data-testid="conversation"
                style={{ display: 'contents', listStyle: 'none', margin: 0, padding: 0 }}
              >
                {messages.map((m, i) => {
                  const answer =
                    m.role === 'assistant'
                      ? answers[messages.slice(0, i).filter((x) => x.role === 'assistant').length]
                      : undefined;
                  return (
                    <li key={i} className="nc-turn" data-role={m.role === 'user' ? 'you' : 'model'}>
                      <span className={m.role === 'user' ? 'nc-sr' : 'nc-who'}>
                        {m.role === 'user' ? 'You' : model}
                      </span>
                      <p>{m.content}</p>
                      {answer && (
                        <div className="nc-meta">
                          <Chip>sealed to its key</Chip>
                          <Chip>usage {units(String(answer.usageMicro))}</Chip>
                          <Chip tone={answer.trust ? 'good' : 'plain'}>
                            {answer.trust
                              ? badgeLabel(answer.trust)
                              : 'signature checked at the end'}
                          </Chip>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ol>
              {seal}
              {settled && (
                <>
                  <p className="nc-divider">
                    <span>Lease closed · charged {units(settled.charge)}</span>
                  </p>
                  <div data-testid="settled" className="nc-settled">
                    <Badge settled={settled} />
                    <dl className="nc-receipt">
                      <div>
                        <dt>Charged</dt>
                        <dd>{units(settled.charge)}</dd>
                      </div>
                      <div>
                        <dt>Reserved, released</dt>
                        <dd>{bound}</dd>
                      </div>
                      <div>
                        <dt>Balance now</dt>
                        <dd>{units(settled.balance)}</dd>
                      </div>
                    </dl>
                    <p data-testid="cost-line" className="nc-cap">
                      Lease cost: {units(settled.charge)} (recomputed from the signed usage and the
                      vault’s rules: {units(settled.expected)}). Balance now{' '}
                      {units(settled.balance)}.
                    </p>
                    {settled.overcharged && (
                      <p className="nc-settled-note" role="alert" data-testid="charge-warning">
                        The operator charged more than one charge step above what the signed usage
                        and the vault’s rules give. Keep this note’s record and consider leaving the
                        operator.
                      </p>
                    )}
                    <Button
                      primary
                      onClick={() => {
                        session.current = null;
                        setSelected(null);
                        setSettled(null);
                        void refresh();
                      }}
                    >
                      New conversation
                    </Button>
                  </div>
                </>
              )}
            </>
          )}
          {preferred !== null &&
            notes.length > 0 &&
            !notes.some((n) => n.record.id === preferred) && (
              <p className="nc-msg" data-tone="warn" role="status" data-testid="chat-note-closed">
                The note you picked is closing or closed, so it cannot start a conversation.{' '}
                <button
                  type="button"
                  className="nc-link"
                  data-testid="chat-note-follow"
                  onClick={() => onLeaving(preferred ?? undefined)}
                >
                  See where it stands
                </button>
              </p>
            )}
          {error && (
            <p className="nc-msg" data-tone="err" role="alert" data-testid="chat-error">
              {error}
            </p>
          )}
          <BackupPrompt
            backup={backup}
            passphrase={passphrase}
            setPassphrase={setPassphrase}
            pass12={pass12}
            onDownload={downloadBackup}
            onChoose={chooseFile}
            hasFile={fileHandle !== null}
          />
          <div ref={threadEnd} />
        </div>
        {idle && (
          <div className="nc-composer">
            <div className="nc-compose-box">
              <textarea
                rows={Math.min(8, Math.max(1, idleDraft.split('\n').length))}
                value={idleDraft}
                placeholder="Ask the model"
                aria-label="Message"
                onChange={(e) => setIdleDraft(e.target.value)}
              />
              <button type="button" className="nc-send" aria-label="Send" disabled>
                <Icon name="right" size={16} />
              </button>
            </div>
            <div className="nc-composer-foot">
              <div className="nc-worst">
                <Strip n={10} solid={0} reserved={10} size="sm" />
                <small>Fund a note to send</small>
              </div>
              <small className="nc-hint">Ctrl or ⌘ + Enter sends</small>
            </div>
          </div>
        )}
        {selected && !settled && (
          <div className="nc-composer" onKeyDown={onKey}>
            <div className="nc-compose-box" data-busy={Boolean(busy)}>
              <textarea
                data-testid="chat-input"
                rows={Math.min(8, Math.max(1, input.split('\n').length))}
                value={input}
                placeholder="Ask the model"
                aria-label="Message"
                onChange={(e) => setInput(e.target.value)}
              />
              <button
                type="button"
                className="nc-send"
                data-testid="chat-send"
                aria-label="Send"
                disabled={Boolean(busy) || !input.trim()}
                onClick={send}
              >
                <Icon name="right" size={16} />
              </button>
            </div>
            <div className="nc-composer-foot">
              <div className="nc-worst">
                <Strip
                  n={10}
                  solid={0}
                  reserved={10}
                  drain={leaseOpen ? clamp(used / boundNum) : 0}
                  size="sm"
                />
                <small>
                  {leaseOpen
                    ? `Used ${units(String(used))} of ${bound}`
                    : `The first message reserves ${bound}`}
                </small>
              </div>
              <small className="nc-hint">Ctrl or ⌘ + Enter sends</small>
            </div>
          </div>
        )}
      </div>
      <InspectorShell label="Note and lease">
        {selected ? (
          <>
            <section className="nc-sum">
              <div className="nc-sum-head">
                <h2>Note #{current?.view.noteId ?? ''}</h2>
                {current && <Status n={noteMark} />}
              </div>
              <p className="nc-big">
                <Count value={current ? Number(BigInt(current.view.balance)) / decimals : 0} />{' '}
                <small>{PROFILE.tokenSymbol}</small>
              </p>
              <Strip
                n={20}
                solid={shareOf(current) * 20}
                size="md"
                label={
                  current
                    ? `${units(current.view.balance)} of ${units(current.record.denomination)} left`
                    : undefined
                }
              />
              <p className="nc-sum-line">
                <span>{current ? `Expires ${formatDate(current.view.expiry)}` : '…'}</span>
                <span>{current ? `of ${units(current.record.denomination)}` : ''}</span>
              </p>
            </section>
            <section className="nc-sum">
              <h2>Lease</h2>
              <Strip
                n={10}
                solid={settled ? clamp(Number(settled.charge) / boundNum) * 10 : 0}
                reserved={leaseOpen ? 10 : 0}
                drain={leaseOpen ? clamp(used / boundNum) : 0}
                size="lg"
                mode={leaseOpen ? undefined : busy ? 'busy' : 'idle'}
                label="Lease bound"
              />
              <p className="nc-sum-line">
                <span>
                  <span data-testid="lease-status">
                    {leaseOpen ? 'open' : settled ? 'settled' : 'not open'}
                  </span>{' '}
                  · bound {bound}
                </span>
                <span>used {units(String(used))}</span>
              </p>
            </section>
            <section className="nc-sum nc-toggles">
              <label className="nc-toggle">
                <input
                  type="checkbox"
                  data-testid="stream"
                  checked={stream}
                  onChange={(e) => setStream(e.target.checked)}
                />
                <span className="nc-toggle-text">
                  <strong>Stream the answer</strong>
                  <small>Expected to come back gateway-signed, not model-signed</small>
                </span>
                <span className="nc-toggle-switch" aria-hidden="true" />
              </label>
              <label className="nc-toggle">
                <input
                  type="checkbox"
                  data-testid="long-job-side"
                  checked={longJob}
                  disabled={leaseOpen || Boolean(busy)}
                  onChange={(e) => setLongJob(e.target.checked)}
                />
                <span className="nc-toggle-text">
                  <strong>Long job</strong>
                  <small>
                    {leaseOpen
                      ? 'Applies to the next lease: end this conversation first'
                      : `${units(String(LONG_JOB_BOUND))} lease class; fewer notes share it, so it is easier to tell apart`}
                  </small>
                </span>
                <span className="nc-toggle-switch" aria-hidden="true" />
              </label>
            </section>
            <Fold
              title="Verification"
              badge={
                <Chip tone={attestation ? 'good' : 'plain'}>
                  {attestation ? CHECK_ROWS.length : 0} of {CHECK_ROWS.length}
                </Chip>
              }
            >
              <VerifyList
                state={(i) =>
                  attestation ? 'pass' : busy && busyKind === 'send' && i === 0 ? 'run' : 'idle'
                }
              />
              {attestation && (
                <p data-testid="attestation-line" className="nc-cap">
                  {attestation}
                </p>
              )}
              {answers.map((a) => (
                <p
                  key={a.id}
                  className="nc-cap"
                  data-testid="answer"
                  data-trust={a.trust?.result ?? 'pending'}
                >
                  Answer {a.id}: {a.promptTokens} in, {a.completionTokens} out tokens, about{' '}
                  {units(String(a.usageMicro))} of usage ·{' '}
                  {a.trust ? badgeLabel(a.trust) : 'signature check pending until the lease ends'}
                </p>
              ))}
            </Fold>
          </>
        ) : idle ? (
          <>
            <section className="nc-sum">
              <h2>Note #41</h2>
              <p className="nc-big">
                <span>0.98</span> <small>{PROFILE.tokenSymbol}</small>
              </p>
              <Strip n={20} solid={19.6} size="md" label="Example: 0.98 of 1 left" />
              <p className="nc-sum-line">
                <span>Expires in 29 days</span>
                <span>of 1 {PROFILE.tokenSymbol}</span>
              </p>
            </section>
            <section className="nc-sum">
              <h2>Lease</h2>
              <Strip n={10} solid={0} size="lg" label="Lease bound" />
              <p className="nc-sum-line">
                <span>not open · bound {bound}</span>
                <span>used 0.000</span>
              </p>
            </section>
            <Fold title="Verification" badge={<Chip>0 of {CHECK_ROWS.length}</Chip>}>
              <VerifyList state={() => 'idle'} />
            </Fold>
          </>
        ) : (
          <section className="nc-sum">
            <h2>How a chat works</h2>
            <Facts
              rows={[
                ['Lease', `${bound} reserved`],
                ['Prompt', 'encrypted to the model’s key'],
                ['End', 'only the charge leaves'],
              ]}
            />
          </section>
        )}
        <Fold title="Who sees what">
          <Facts
            rows={[
              [
                'Operator',
                'proof, nullifier, lease class, model, settings, message sizes, usage, timing',
              ],
              ['NEAR AI', 'token counts and timing; your prompt only as the model’s enclave'],
              ['Chain', 'the deposit, then nothing until you close or exit'],
            ]}
          />
          <p className="nc-cap" data-testid="chat-limits">
            This web app cannot hide your IP address from the operator, NEAR AI’s gateway or an RPC
            provider, and the browser cannot use Tor. Use Tor Browser or <code>nymirad</code> for
            that.
          </p>
        </Fold>
        <Fold title="About the lease">
          <p className="nc-cap">
            A lease reserves its bound while it is open. When it closes, only the charge leaves the
            note. The proof for the first lease is made while you type.
          </p>
          <p data-testid="prover-status" className="nc-cap" data-status={prover.status}>
            {prover.status === 'loading' && (
              <>
                <WaitBar /> Loading the proving key (SHA-256 checked)…
              </>
            )}
            {prover.status === 'ready' &&
              `Proving key loaded and checked (${(prover.bytes / 1e6).toFixed(1)} MB, ${prover.ms} ms).`}
            {prover.status === 'failed' && `The proving key was refused: ${prover.reason}`}
          </p>
        </Fold>
        {selected && (
          <div className="nc-sum nc-privacy-link">
            <Button testId="chat-privacy" onClick={() => onPrivacy(selected)}>
              <Icon name="shield" size={14} /> Privacy panel
            </Button>
          </div>
        )}
      </InspectorShell>
    </section>
  );
}

function BackupPrompt(p: {
  backup: { needed: boolean; leases: number } | null;
  passphrase: string;
  setPassphrase: (v: string) => void;
  pass12: boolean;
  onDownload: () => void;
  onChoose: () => void;
  hasFile: boolean;
}) {
  const canPick = fsAccess();
  const [same, setSame] = useState(false);
  if (!p.backup?.needed && !(canPick && !p.hasFile && p.backup)) return null;
  return (
    <div data-testid="backup-prompt" className="nc-chat-fund" role="status">
      <p>
        {canPick
          ? 'Your note changed. Choose a file once and the app rewrites an encrypted backup after every lease.'
          : 'Your note changed. Download an encrypted backup now: a lost latest state lets the operator defeat your exit and can cost the remaining balance. This browser cannot write to a chosen file; nymirad or a Chromium browser backs up automatically.'}
      </p>
      <SameAsUnlock
        offered={typedUnlockPassphrase() !== null}
        testId="backup-same"
        on={same}
        onChange={(on) => {
          setSame(on);
          p.setPassphrase(on ? (typedUnlockPassphrase() ?? '') : '');
        }}
      />
      {!same && (
        <label className="nc-field">
          <span>
            Backup file passphrase (at least {MIN_PASSPHRASE_LENGTH} characters; separate from your
            unlock key; not in a synced folder)
          </span>
          <input
            type="password"
            data-testid="backup-passphrase"
            value={p.passphrase}
            onChange={(e) => p.setPassphrase(e.target.value)}
          />
        </label>
      )}
      <div>
        {canPick ? (
          <Button testId="backup-choose" disabled={!p.pass12} onClick={p.onChoose}>
            Choose backup file
          </Button>
        ) : (
          <Button testId="backup-download" disabled={!p.pass12} onClick={p.onDownload}>
            Download backup
          </Button>
        )}
      </div>
    </div>
  );
}
