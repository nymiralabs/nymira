import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { loadNotes, makeBackupFile } from '../funding/flow';
import { noteAad, type NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { backupDue } from '../chat/backup-policy';
import { retireBackup } from '../chat/retire-backup';
import { DEFAULT_BOUND, DEFAULT_MODEL, LONG_JOB_BOUND, MODELS } from '../chat/models';
import { Cells, Cipher } from '../marketing/Board';
import { chatDeps, warmProver, type ProverState } from '../chat/runtime';
import { ChatError, ChatSession, type Answer, type Message, type Settled } from '../chat/session';
import { badgeLabel } from '../chat/attest/signature';
import type { NoteView } from '../note/lease';
import { download, formatDate, formatUnits } from './format';
import { leaseBlocked } from '../leave/guard';
import { leaveStore } from '../leave/store';
import { Unlock } from './Unlock';
import type { VaultCheck } from '../chain/vault';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;

interface FsHandle {
  createWritable(): Promise<{ write(data: BlobPart): Promise<void>; close(): Promise<void> }>;
}
type PickerWindow = Window & {
  showSaveFilePicker?: (o: unknown) => Promise<FsHandle>;
};

const nowSecs = () => Math.floor(Date.now() / 1000);
const fsAccess = () => typeof (window as PickerWindow).showSaveFilePicker === 'function';

function Badge({ settled }: { settled: Settled }) {
  const t = settled.trust;
  return (
    <div
      data-testid="badge"
      data-kind={t.result}
      className={`nm-app-badge ${t.result === 'not_verified' ? 'warning' : 'ok'}`}
    >
      <p>
        <strong data-testid="badge-text">{badgeLabel(t)}</strong>
        {t.result === 'not_verified' && <> ({t.reason})</>}
      </p>
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
}: {
  vault: VaultCheck | null;
  onPrivacy: (noteId: string) => void;
}) {
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const [notes, setNotes] = useState<{ record: NoteRecord; view: NoteView }[]>([]);
  const [prover, setProver] = useState<ProverState>({ status: 'loading' });
  const [selected, setSelected] = useState<string | null>(null);
  const [model, setModel] = useState(DEFAULT_MODEL);
  const [longJob, setLongJob] = useState(false);
  const [stream, setStream] = useState(false);
  const [input, setInput] = useState('');
  const [messages, setMessages] = useState<Message[]>([]);
  const [answers, setAnswers] = useState<Answer[]>([]);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [settled, setSettled] = useState<Settled | null>(null);
  const [attestation, setAttestation] = useState('');
  const backup = useSyncExternalStore(retireBackup.subscribe, () => retireBackup.state);
  const [passphrase, setPassphrase] = useState('');
  const [fileHandle, setFileHandle] = useState<FsHandle | null>(null);
  const session = useRef<ChatSession | null>(null);
  const leave = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);

  useEffect(() => {
    warmProver().then(setProver);
  }, []);

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

  if (!unlocked) return <Unlock onReady={() => setUnlocked(true)} />;

  if (prover.status === 'off') {
    return (
      <section data-testid="chat-off" className="nm-app-plain-screen">
        <h1>Chat</h1>
        <p className="warning">{prover.reason} Chat is off in this build.</p>
      </section>
    );
  }

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

  const send = async () => {
    const s = session.current;
    if (!s || !input.trim()) return;
    setError('');
    const next: Message[] = [...messages, { role: 'user', content: input }];
    setMessages(next);
    setInput('');
    try {
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
  const bound = units(longJob ? LONG_JOB_BOUND : DEFAULT_BOUND);
  const current = selected ? notes.find((n) => n.record.id === selected) : undefined;
  // The share of the deposit still on the note, in tenths, for the cell bar.
  const tenths = (n?: { record: NoteRecord; view: NoteView }) => {
    if (!n) return { filled: 0, part: 0 };
    try {
      const share = Number(BigInt(n.view.balance)) / Number(BigInt(n.record.denomination));
      const t = Math.max(0, Math.min(1, share)) * 10;
      return { filled: Math.floor(t), part: t % 1 };
    } catch {
      return { filled: 0, part: 0 };
    }
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !busy) {
      e.preventDefault();
      void send();
    }
  };

  return (
    <section data-testid="chat-screen" className="nm-app-body nm-app-chat-screen">
      <div className="nm-app-main">
        <h1>Chat</h1>
        <p className="nm-app-lead">
          Ask an attested model. Your prompt is encrypted to its key before it leaves this page.
        </p>
        <p data-testid="prover-status" className="hint nm-app-status" data-status={prover.status}>
          {prover.status === 'loading' && 'Loading the proving key (SHA-256 checked)…'}
          {prover.status === 'ready' &&
            `Proving key loaded and checked (${(prover.bytes / 1e6).toFixed(1)} MB, ${prover.ms} ms).`}
          {prover.status === 'failed' && `The proving key was refused: ${prover.reason}`}
        </p>
        {vault?.status === 'mismatch' && (
          <p className="warning" role="alert">
            The vault differs from what this build expects, so no new lease will open.
          </p>
        )}
        {leave.emergency && !selected && (
          <p className="warning" role="alert" data-testid="chat-emergency">
            The operator posted an emergency notice. No new lease is opened. Use the Close screen to
            request an exit.
          </p>
        )}
        {!selected && (
          <div data-testid="chat-notes" className="nm-app-picker">
            {notes.length === 0 && (
              <p className="nm-app-empty">No funded note yet. Fund one first.</p>
            )}
            <ul className="nm-app-note-cards">
              {notes.map(({ record, view }) => {
                const t = tenths({ record, view });
                const near = leave.settings.enabled && leaseBlocked(view.expiry, nowSecs());
                return (
                  <li key={record.id} data-testid="chat-note" className="nm-app-note-card">
                    <div className="nm-app-note-top">
                      <span>
                        <strong className="nm-app-note-balance">{units(view.balance)}</strong>
                        <small>
                          note #{view.noteId} · expires {formatDate(view.expiry)}
                        </small>
                      </span>
                      {view.pending && (
                        <em data-testid="note-pending" className="nm-app-chip">
                          (a lease is not settled)
                        </em>
                      )}
                    </div>
                    <Cells n={10} filled={t.filled} part={t.part} />
                    <div className="nm-app-note-actions">
                      {view.pending ? (
                        <button
                          type="button"
                          data-testid="chat-settle"
                          className="primary"
                          onClick={() => settleOpen(record)}
                        >
                          Settle the open lease
                        </button>
                      ) : (
                        <button
                          type="button"
                          data-testid="chat-use-note"
                          className="primary"
                          disabled={prover.status !== 'ready' || leave.emergency !== null || near}
                          onClick={() => start(record)}
                        >
                          Chat with this note
                        </button>
                      )}
                      <button
                        type="button"
                        data-testid="chat-privacy"
                        onClick={() => onPrivacy(record.id)}
                      >
                        Privacy panel
                      </button>
                    </div>
                    {near && (
                      <em data-testid="note-near-expiry" className="nm-app-note-warn">
                        (expires within 5 days: no new lease; close it or request an exit)
                      </em>
                    )}
                  </li>
                );
              })}
            </ul>
            <label className="nm-app-toggle">
              <input
                type="checkbox"
                data-testid="long-job"
                checked={longJob}
                onChange={(e) => setLongJob(e.target.checked)}
              />
              <span>
                Long job
                <small>
                  0.50 USDC lease class; fewer notes share it, so it is easier to tell apart
                </small>
              </span>
            </label>
          </div>
        )}
        {selected && (
          <>
            <div className="nm-app-controls">
              <label className="nm-app-field">
                <span>Model</span>
                <select
                  data-testid="model-select"
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                >
                  {MODELS.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.id}
                    </option>
                  ))}
                </select>
              </label>
              <label className="nm-app-toggle">
                <input
                  type="checkbox"
                  data-testid="stream"
                  checked={stream}
                  onChange={(e) => setStream(e.target.checked)}
                />
                <span>
                  Stream the answer
                  <small>Expected to come back gateway-signed, not model-signed</small>
                </span>
              </label>
            </div>
            <ol data-testid="conversation" className="nm-chat-thread">
              {messages.map((m, i) => (
                <li key={i} data-role={m.role} className="nm-app-turn">
                  <strong>{m.role === 'user' ? 'You' : 'Model'}:</strong>{' '}
                  <span className="nm-app-turn-text">{m.content}</span>
                </li>
              ))}
            </ol>
            {messages.length === 0 && !busy && (
              <p className="nm-app-empty">
                Your first message opens a lease that reserves {bound}. The proof for it is made
                while you type.
              </p>
            )}
            {busy && (
              <div className="nm-app-busy" role="status">
                <Cipher run rows={2} cols={36} />
                <p data-testid="chat-busy">{busy}</p>
              </div>
            )}
            {!settled && (
              <div className="nm-chat-composer" onKeyDown={onKey}>
                <textarea
                  data-testid="chat-input"
                  value={input}
                  rows={3}
                  placeholder="Ask the model"
                  onChange={(e) => setInput(e.target.value)}
                  aria-label="Message"
                />
                <div className="nm-chat-composer-bar">
                  <small>Ctrl or ⌘ + Enter sends</small>
                  <span>
                    <button
                      type="button"
                      data-testid="chat-end"
                      disabled={Boolean(busy) || !leaseOpen}
                      onClick={end}
                    >
                      End conversation
                    </button>
                    <button
                      type="button"
                      data-testid="chat-send"
                      className="primary"
                      disabled={Boolean(busy)}
                      onClick={send}
                    >
                      Send
                    </button>
                  </span>
                </div>
              </div>
            )}
            {settled && (
              <div data-testid="settled" className="nm-app-settled">
                <Badge settled={settled} />
                <p data-testid="cost-line">
                  Lease cost: {units(settled.charge)} (recomputed from the signed usage and the
                  vault’s rules: {units(settled.expected)}). Balance now {units(settled.balance)}.
                </p>
                {settled.overcharged && (
                  <p className="warning" role="alert" data-testid="charge-warning">
                    The operator charged more than one charge step above what the signed usage and
                    the vault’s rules give. Keep this note’s record and consider leaving the
                    operator.
                  </p>
                )}
                <button
                  type="button"
                  className="primary"
                  onClick={() => {
                    session.current = null;
                    setSelected(null);
                    setSettled(null);
                    void refresh();
                  }}
                >
                  New conversation
                </button>
              </div>
            )}
          </>
        )}
        {!selected && busy && (
          <div className="nm-app-busy" role="status">
            <Cipher run rows={2} cols={36} />
            <p data-testid="chat-busy">{busy}</p>
          </div>
        )}
        {error && (
          <p className="error" role="alert" data-testid="chat-error">
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
      </div>
      <aside className="nm-app-side" aria-label="Note and lease">
        {selected ? (
          <>
            <h2>Note #{current?.view.noteId ?? ''}</h2>
            <dl className="nm-app-facts">
              <div>
                <dt>Balance at start</dt>
                <dd>{current ? units(current.view.balance) : '…'}</dd>
              </div>
              <div>
                <dt>Expires</dt>
                <dd>{current ? formatDate(current.view.expiry) : '…'}</dd>
              </div>
            </dl>
            <Cells n={10} filled={tenths(current).filled} part={tenths(current).part} />
            <p className="nm-app-cap">Each cell is a tenth of the deposit.</p>
            <h2>Lease</h2>
            <dl className="nm-app-facts">
              <div>
                <dt>Status</dt>
                <dd data-testid="lease-status">{leaseOpen ? 'open' : 'not open'}</dd>
              </div>
              <div>
                <dt>Bound</dt>
                <dd>{bound}</dd>
              </div>
            </dl>
            <p className="nm-app-cap">
              The bound is reserved while the lease is open. Only the charge leaves the note.
            </p>
            {attestation && (
              <>
                <h2>Attestation</h2>
                <p data-testid="attestation-line" className="hint">
                  {attestation}
                </p>
              </>
            )}
            {answers.length > 0 && <h2>Answers</h2>}
            {answers.map((a) => (
              <p
                key={a.id}
                className="hint nm-app-answer"
                data-testid="answer"
                data-trust={a.trust?.result ?? 'pending'}
              >
                Answer {a.id}: {a.promptTokens} in, {a.completionTokens} out tokens, about{' '}
                {units(String(a.usageMicro))} of usage ·{' '}
                {a.trust ? badgeLabel(a.trust) : 'signature check pending until the lease ends'}
              </p>
            ))}
            <button type="button" data-testid="chat-privacy" onClick={() => onPrivacy(selected)}>
              Privacy panel
            </button>
          </>
        ) : (
          <>
            <h2>How a chat works</h2>
            <dl className="nm-app-facts">
              <div>
                <dt>Lease</dt>
                <dd>{bound} reserved</dd>
              </div>
              <div>
                <dt>Prompt</dt>
                <dd>encrypted to the model’s key</dd>
              </div>
              <div>
                <dt>End</dt>
                <dd>only the charge leaves</dd>
              </div>
            </dl>
          </>
        )}
        <p className="hint" data-testid="chat-limits">
          This web app cannot hide your IP address from the operator, NEAR AI’s gateway or an RPC
          provider, and the browser cannot use Tor. Use Tor Browser or <code>nymirad</code> for
          that.
        </p>
      </aside>
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
  if (!p.backup?.needed && !(canPick && !p.hasFile && p.backup)) return null;
  return (
    <div data-testid="backup-prompt" className="notice">
      <p>
        {canPick
          ? 'Your note changed. Choose a file once and the app rewrites an encrypted backup after every lease.'
          : 'Your note changed. Download an encrypted backup now: a lost latest state lets the operator defeat your exit and can cost the remaining balance. This browser cannot write to a chosen file; nymirad or a Chromium browser backs up automatically.'}
      </p>
      <label>
        Backup passphrase (at least {MIN_PASSPHRASE_LENGTH} characters; not in a synced folder){' '}
        <input
          type="password"
          data-testid="backup-passphrase"
          value={p.passphrase}
          onChange={(e) => p.setPassphrase(e.target.value)}
        />
      </label>{' '}
      {canPick ? (
        <button type="button" data-testid="backup-choose" disabled={!p.pass12} onClick={p.onChoose}>
          Choose backup file
        </button>
      ) : (
        <button
          type="button"
          data-testid="backup-download"
          disabled={!p.pass12}
          onClick={p.onDownload}
        >
          Download backup
        </button>
      )}
    </div>
  );
}
