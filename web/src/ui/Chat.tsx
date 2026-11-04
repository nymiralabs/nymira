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
import { DEFAULT_MODEL, MODELS } from '../chat/models';
import { chatDeps, warmProver, type ProverState } from '../chat/runtime';
import { ChatError, ChatSession, type Answer, type Message, type Settled } from '../chat/session';
import { badgeLabel } from '../chat/attest/signature';
import type { NoteView } from '../note/lease';
import { download, formatUnits } from './format';
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
      className={t.result === 'not_verified' ? 'warning' : 'ok'}
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

export function Chat({ vault }: { vault: VaultCheck | null }) {
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
      <section data-testid="chat-off">
        <h2>Chat</h2>
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

  return (
    <section data-testid="chat-screen">
      <h2>Chat</h2>
      <p data-testid="prover-status" className="hint">
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
        <div data-testid="chat-notes">
          {notes.length === 0 && <p>No funded note yet. Fund one first.</p>}
          <ul>
            {notes.map(({ record, view }) => (
              <li key={record.id} data-testid="chat-note">
                {units(view.balance)} · note #{view.noteId}{' '}
                {view.pending && <em data-testid="note-pending">(a lease is not settled)</em>}
                {view.pending ? (
                  <button
                    type="button"
                    data-testid="chat-settle"
                    onClick={() => settleOpen(record)}
                  >
                    Settle the open lease
                  </button>
                ) : (
                  <button
                    type="button"
                    data-testid="chat-use-note"
                    disabled={
                      prover.status !== 'ready' ||
                      leave.emergency !== null ||
                      (leave.settings.enabled && leaseBlocked(view.expiry, nowSecs()))
                    }
                    onClick={() => start(record)}
                  >
                    Chat with this note
                  </button>
                )}
                {leave.settings.enabled && leaseBlocked(view.expiry, nowSecs()) && (
                  <em data-testid="note-near-expiry">
                    {' '}
                    (expires within 5 days: no new lease; close it or request an exit)
                  </em>
                )}
              </li>
            ))}
          </ul>
          <label>
            <input
              type="checkbox"
              data-testid="long-job"
              checked={longJob}
              onChange={(e) => setLongJob(e.target.checked)}
            />{' '}
            Long job (0.50 USDC lease class; fewer notes share it, so it is easier to tell apart)
          </label>
        </div>
      )}
      {selected && (
        <>
          <div>
            <label>
              Model{' '}
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
            </label>{' '}
            <label>
              <input
                type="checkbox"
                data-testid="stream"
                checked={stream}
                onChange={(e) => setStream(e.target.checked)}
              />{' '}
              Stream the answer (expected to come back gateway-signed, not model-signed)
            </label>
          </div>
          <ol data-testid="conversation">
            {messages.map((m, i) => (
              <li key={i} data-role={m.role}>
                <strong>{m.role === 'user' ? 'You' : 'Model'}:</strong> {m.content}
              </li>
            ))}
          </ol>
          {answers.map((a) => (
            <p
              key={a.id}
              className="hint"
              data-testid="answer"
              data-trust={a.trust?.result ?? 'pending'}
            >
              Answer {a.id}: {a.promptTokens} in, {a.completionTokens} out tokens, about{' '}
              {units(String(a.usageMicro))} of usage ·{' '}
              {a.trust ? badgeLabel(a.trust) : 'signature check pending until the lease ends'}
            </p>
          ))}
          {attestation && (
            <p data-testid="attestation-line" className="hint">
              {attestation}
            </p>
          )}
          {!settled && (
            <>
              <textarea
                data-testid="chat-input"
                value={input}
                rows={3}
                onChange={(e) => setInput(e.target.value)}
                aria-label="Message"
              />
              <p>
                <button
                  type="button"
                  data-testid="chat-send"
                  disabled={Boolean(busy)}
                  onClick={send}
                >
                  Send
                </button>{' '}
                <button
                  type="button"
                  data-testid="chat-end"
                  disabled={Boolean(busy) || !leaseOpen}
                  onClick={end}
                >
                  End conversation
                </button>
              </p>
            </>
          )}
          {settled && (
            <div data-testid="settled">
              <Badge settled={settled} />
              <p data-testid="cost-line">
                Lease cost: {units(settled.charge)} (recomputed from the signed usage and the
                vault’s rules: {units(settled.expected)}). Balance now {units(settled.balance)}.
              </p>
              {settled.overcharged && (
                <p className="warning" role="alert" data-testid="charge-warning">
                  The operator charged more than one charge step above what the signed usage and the
                  vault’s rules give. Keep this note’s record and consider leaving the operator.
                </p>
              )}
              <button
                type="button"
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
      {busy && <p data-testid="chat-busy">{busy}</p>}
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
      <p className="hint" data-testid="chat-limits">
        This web app cannot hide your IP address from the operator, NEAR AI’s gateway or an RPC
        provider, and the browser cannot use Tor. Use Tor Browser or <code>nymirad</code> for that.
      </p>
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
