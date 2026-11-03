import { Fragment, useEffect, useState, useSyncExternalStore } from 'react';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { loadNotes, makeBackupFile } from '../funding/flow';
import { warmWithdrawalProver, chatDeps, type ProverState } from '../chat/runtime';
import { ChatSession } from '../chat/session';
import {
  CLAIM_NOTE,
  CLOSE_REVEALS,
  GAS_NOTE,
  OLD_STATE_LIMIT,
  OPERATOR_CLOSE_NOTE,
  PUBLIC_SENTENCE,
} from '../leave/text';
import { leaveStore, type Row } from '../leave/store';
import { noteAad, type NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { download, formatDate, formatUnits, readFile } from './format';
import { rpc, useWallet } from './services';
import { Unlock } from './Unlock';
import { WalletConnect } from './WalletConnect';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;

/** Text with `code` spans: backticks in the fixed wording become <code>. */
export function Ticks({ text }: { text: string }) {
  return (
    <>
      {text.split('`').map((part, i) => (
        <Fragment key={i}>{i % 2 === 1 ? <code>{part}</code> : part}</Fragment>
      ))}
    </>
  );
}

const STATE_TEXT: Record<string, string> = {
  closing: 'close started, not confirmed',
  closed: 'closed',
  exiting: 'exit requested',
  exited: 'exit finalized',
};

function when(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toISOString().replace('T', ' ').slice(0, 16) + ' UTC';
}

function Countdown({ seconds }: { seconds: number }) {
  if (seconds <= 0) return <>now</>;
  const d = Math.floor(seconds / 86_400);
  const h = Math.floor((seconds % 86_400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return (
    <>
      in {d > 0 ? `${d} d ` : ''}
      {h} h {m} min
    </>
  );
}

function BackupAfterClose({ ids }: { ids: string[] }) {
  const [pass, setPass] = useState('');
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [tested, setTested] = useState(false);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const ok = passphraseLength(pass) >= MIN_PASSPHRASE_LENGTH;
  if (ids.length === 0) return null;

  const downloadIt = async () => {
    setError('');
    setBusy(true);
    try {
      const file = await makeBackupFile(pass);
      setBytes(file);
      download(file as BlobPart, backupFileName(new Date()), 'application/octet-stream');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const testIt = async (file: File | undefined) => {
    if (!file) return;
    setError('');
    setBusy(true);
    try {
      const chosen = await readFile(file);
      const records = (await loadNotes()).filter((n) => ids.includes(n.id));
      for (const record of records) {
        const check = await callWorker({
          op: 'testBackup',
          key: requireKey(),
          bytes: chosen,
          passphrase: pass,
          record,
        });
        if (!check.matches)
          throw new Error('The file does not hold this note. Download a new one.');
      }
      setTested(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="notice" data-testid="close-backup">
      <p>
        Download an encrypted backup of this note’s final state now, then choose the file again so
        the app can test it. A backup you cannot open is no backup. Do not keep it in a synced
        folder.
      </p>
      <label>
        Backup passphrase (at least {MIN_PASSPHRASE_LENGTH} characters){' '}
        <input
          type="password"
          data-testid="close-backup-passphrase"
          value={pass}
          onChange={(e) => setPass(e.target.value)}
        />
      </label>{' '}
      <button
        type="button"
        data-testid="close-backup-download"
        disabled={!ok || busy}
        onClick={downloadIt}
      >
        Download backup
      </button>
      {bytes && (
        <label>
          Choose the downloaded file to test it{' '}
          <input
            type="file"
            data-testid="close-backup-file"
            disabled={busy}
            onChange={(e) => {
              const file = e.target.files?.[0];
              e.target.value = '';
              void testIt(file);
            }}
          />
        </label>
      )}
      {tested && (
        <p className="ok" data-testid="close-backup-tested">
          The backup opened and holds this note’s state.{' '}
          <button
            type="button"
            data-testid="close-backup-done"
            onClick={() => ids.forEach((id) => leaveStore.dismissBackup(id))}
          >
            Done
          </button>
        </p>
      )}
      {error && (
        <p className="error" role="alert" data-testid="close-backup-error">
          {error}
        </p>
      )}
    </div>
  );
}

function NoteCard({
  row,
  chainNow,
  prover,
}: {
  row: Row;
  chainNow: number | null;
  prover: boolean;
}) {
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();
  const { record, view } = row;
  const leave = record.leave;
  const busy = s.busy !== null;
  const mine = s.busy?.id === record.id;
  const pending = s.pending[record.id];
  const connected = wallet.connected !== null;
  const canAct = prover && !busy;

  const settle = async () => {
    // Best effort, like `nymirad`: an unsettled lease is finished if the operator can.
    const deps = await chatDeps(requireKey);
    const session = new ChatSession(deps, record);
    await session.resume({ pollMs: 500, maxWaitMs: 20_000 });
  };

  return (
    <li data-testid="leave-note" data-state={leave?.state ?? 'open'}>
      <p>
        <strong>{units(view.balance)}</strong> · note #{view.noteId} · expires{' '}
        {formatDate(view.expiry)} · payout{' '}
        <strong data-testid="leave-payout">{record.payout}</strong>
        {' · '}
        <span data-testid="leave-state">{leave ? STATE_TEXT[leave.state] : 'open'}</span>
      </p>
      {view.active?.token !== undefined && (
        <p className="hint" data-testid="leave-lease-open">
          A lease is open on this note. End the conversation on the Chat screen before closing.
        </p>
      )}
      {view.pending && view.active?.token === undefined && !leave && (
        <p className="hint" data-testid="leave-pending">
          An earlier lease is not settled yet. A close needs it settled first (Chat screen, “Settle
          the open lease”). An exit goes ahead and concedes the lease’s bound.
        </p>
      )}

      {(!leave || leave.state === 'closing') && (
        <div>
          <p>
            <button
              type="button"
              data-testid="close-wallet"
              disabled={!canAct || !connected}
              onClick={() => void leaveStore.close(record, 'wallet')}
            >
              {leave?.state === 'closing' ? 'Continue the close' : 'Close note'} (sign with your
              wallet, no close fee)
            </button>{' '}
            <button
              type="button"
              data-testid="close-operator"
              disabled={!canAct}
              onClick={() => void leaveStore.close(record, 'operator')}
            >
              Close through the operator (it pays the gas and takes the close fee)
            </button>
          </p>
          <p className="hint" data-testid="operator-close-note">
            {OPERATOR_CLOSE_NOTE}
          </p>
          {leave?.state === 'closing' && (
            <p>
              <button
                type="button"
                data-testid="close-recheck"
                disabled={!canAct}
                onClick={() => void leaveStore.recheckClosed(record)}
              >
                Ask the vault whether the close landed
              </button>
            </p>
          )}
          <p>
            <button
              type="button"
              data-testid="exit-request"
              disabled={!canAct || !connected}
              onClick={() => void leaveStore.exit(record, view.pending ? settle : undefined)}
            >
              Request exit (the operator is not cooperating)
            </button>
          </p>
          <p className="hint" data-testid="exit-note">
            An exit sends <code>request_exit</code>. It concedes{' '}
            <strong data-testid="exit-concession">{units(view.boundAtState)}</strong> to the
            operator: 0 when no request left this note’s latest state, the stored bound of that
            state otherwise. After the exit proof is made, this note makes no new request.
          </p>
        </div>
      )}

      {leave?.state === 'exiting' && (
        <div data-testid="exit-panel">
          {leave.deadline !== undefined ? (
            <p>
              Exit deadline: <strong data-testid="exit-deadline">{when(leave.deadline)}</strong>
              {chainNow !== null && (
                <>
                  {' '}
                  (<Countdown seconds={leave.deadline - chainNow} /> by the chain’s clock)
                </>
              )}
              . After it, <code>finalize_exit</code> pays your payout account.
            </p>
          ) : (
            <p data-testid="exit-unconfirmed">
              The exit request was sent, but the vault does not list it yet.{' '}
              <button
                type="button"
                data-testid="exit-recheck"
                disabled={busy}
                onClick={() => void leaveStore.refreshExitState(record)}
              >
                Ask the vault again
              </button>
            </p>
          )}
          <p>
            <button
              type="button"
              data-testid="exit-finalize"
              disabled={
                !canAct ||
                !connected ||
                leave.deadline === undefined ||
                chainNow === null ||
                chainNow < leave.deadline
              }
              onClick={() => void leaveStore.finalize(record)}
            >
              Finalize exit
            </button>
          </p>
        </div>
      )}

      {(leave?.state === 'closed' || leave?.state === 'exited') && (
        <div data-testid="paid-panel">
          <p>
            The vault pays <strong>{record.payout}</strong>. If that transfer could not be sent, the
            money waits in the vault until <code>claim_payout</code>.
          </p>
          <p>
            <button
              type="button"
              data-testid="claim-check"
              disabled={busy}
              onClick={() => void leaveStore.readPending(record)}
            >
              Check for a pending payout
            </button>{' '}
            <button
              type="button"
              data-testid="claim-payout"
              disabled={!canAct || !connected}
              onClick={() => void leaveStore.claim(record)}
            >
              Claim payout
            </button>
          </p>
          {pending !== undefined && (
            <p data-testid="claim-pending">
              {pending === '0'
                ? `Nothing is pending for ${record.payout}.`
                : `${units(pending)} is pending for ${record.payout}.`}
            </p>
          )}
          <p className="hint" data-testid="claim-note">
            <Ticks text={CLAIM_NOTE} />
          </p>
        </div>
      )}
      {mine && s.busy && (
        <p data-testid="leave-busy" aria-live="polite">
          {s.busy.text}
        </p>
      )}
    </li>
  );
}

export function Close() {
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();
  const [prover, setProver] = useState<ProverState>({ status: 'loading' });
  const [chainNow, setChainNow] = useState<number | null>(null);

  useEffect(() => {
    warmWithdrawalProver().then(setProver);
  }, []);

  useEffect(() => {
    if (!unlocked) return;
    let live = true;
    const read = async () => {
      await leaveStore.refresh().catch(() => undefined);
      void leaveStore.checkEmergency(true);
      rpc.finalBlockTime().then(
        (t) => live && setChainNow(t),
        () => undefined,
      );
    };
    void read();
    // The chain's clock moves while the screen is open: the finalize button follows it.
    const timer = setInterval(() => void read(), 30_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [unlocked, s.busy === null]);

  useEffect(() => {
    if (unlocked && wallet.connected) void leaveStore.tick();
  }, [unlocked, wallet.connected]);

  if (!unlocked) return <Unlock onReady={() => setUnlocked(true)} />;

  const advice = leaveStore.emergencyAdvice();
  const rows = s.rows;

  return (
    <section data-testid="close-screen">
      <h2>Close or exit a note</h2>
      <blockquote data-testid="public-sentence">
        <Ticks text={PUBLIC_SENTENCE} />
      </blockquote>
      <p data-testid="old-state-limit">{OLD_STATE_LIMIT}</p>
      <p data-testid="close-reveals">{CLOSE_REVEALS}</p>
      <p className="hint" data-testid="gas-note">
        <Ticks text={GAS_NOTE} />
      </p>
      {advice && (
        <p className="warning" role="alert" data-testid="emergency-advice">
          {advice}
        </p>
      )}
      <h3>Wallet</h3>
      <WalletConnect busy={s.busy !== null} />
      <p className="hint" data-testid="withdrawal-prover-status">
        {prover.status === 'loading' && 'Loading the withdrawal proving key (SHA-256 checked)…'}
        {prover.status === 'ready' &&
          `Withdrawal proving key loaded and checked (${(prover.bytes / 1e6).toFixed(1)} MB, ${prover.ms} ms).`}
        {prover.status === 'off' && `${prover.reason} Close and exit are off in this build.`}
        {prover.status === 'failed' && `The withdrawal proving key was refused: ${prover.reason}`}
      </p>

      <h3>Notes</h3>
      {rows.length === 0 ? (
        <p data-testid="no-open-notes">No funded note in this browser.</p>
      ) : (
        <ul className="notes">
          {rows.map((row) => (
            <NoteCard
              key={row.record.id}
              row={row}
              chainNow={chainNow}
              prover={prover.status === 'ready'}
            />
          ))}
        </ul>
      )}
      {s.message && (
        <p className="ok" role="status" data-testid="leave-message">
          {s.message}
        </p>
      )}
      {s.error && (
        <p className="error" role="alert" data-testid="leave-error">
          {s.error}
        </p>
      )}
      <BackupAfterClose ids={s.backupFor} />

      <h3>Expiry</h3>
      <p className="hint">
        A note that reaches its expiry goes to the treasury. While this app is open it shows notices
        at 14, 7 and 2 days, stops opening leases on the note 5 days before, starts a close at 5
        days and, if the close did not go through, an exit at 3 days. Each waits for your wallet
        signature.
      </p>
      <label>
        <input
          type="checkbox"
          data-testid="guard-enabled"
          checked={s.settings.enabled}
          onChange={(e) =>
            void leaveStore.setSettings({ ...s.settings, enabled: e.target.checked })
          }
        />{' '}
        Expiry guard (on by default)
      </label>
      <br />
      <label>
        <input
          type="checkbox"
          data-testid="close-at-end"
          checked={s.settings.closeAtSessionEnd}
          onChange={(e) =>
            void leaveStore.setSettings({ ...s.settings, closeAtSessionEnd: e.target.checked })
          }
        />{' '}
        Start a close when a conversation ends (off by default: every close shows the note ID and
        the total spend on chain)
      </label>
    </section>
  );
}
