import './close.css';
import { Fragment, useEffect, useState, useSyncExternalStore } from 'react';
import closeIllustration from '../marketing/assets/app-close.webp';
import {
  Button,
  Card,
  Chip,
  Fold,
  Screen,
  Status,
  type NoteStatus,
  Strip,
  WaitBar,
  Waiting,
  clamp,
} from '../marketing/kit';
import { ExitDays, GuardTimeline, LeavePath } from '../marketing/kit-close';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { SameAsUnlock } from '../marketing/kit';
import { isUnlocked, requireKey, typedUnlockPassphrase } from '../crypto/wrap';
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
import { CLOSE_DAYS } from '../leave/guard';
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
  const [same, setSame] = useState(false);
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
    <Card title="Back up the final state" testId="close-backup">
      <p className="nc-cap nc-cap-first">
        Download an encrypted backup of this note’s final state now, then choose the file again so
        the app can test it. A backup you cannot open is no backup. Do not keep it in a synced
        folder. The file passphrase is separate from your unlock key: it must work on any device.
      </p>
      <SameAsUnlock
        offered={typedUnlockPassphrase() !== null}
        testId="close-backup-same"
        on={same}
        onChange={(on) => {
          setSame(on);
          setPass(on ? (typedUnlockPassphrase() ?? '') : '');
        }}
      />
      {!same && (
        <label className="nc-field">
          <span>Backup file passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)</span>
          <input
            type="password"
            data-testid="close-backup-passphrase"
            value={pass}
            onChange={(e) => setPass(e.target.value)}
          />
        </label>
      )}
      <div className="nc-actions">
        <Button primary testId="close-backup-download" disabled={!ok || busy} onClick={downloadIt}>
          Download backup
        </Button>
      </div>
      {bytes && (
        <label className="nc-field">
          <span>Choose the downloaded file to test it</span>
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
        <div className="nc-done" data-testid="close-backup-tested">
          <Chip tone="good">tested</Chip>
          <p>The backup opened and holds this note’s state.</p>
          <Button
            testId="close-backup-done"
            onClick={() => ids.forEach((id) => leaveStore.dismissBackup(id))}
          >
            Done
          </Button>
        </div>
      )}
      {error && (
        <p className="nc-msg" data-tone="err" role="alert" data-testid="close-backup-error">
          {error}
        </p>
      )}
    </Card>
  );
}

/** Share of the deposit still on the note, 0 to 1. */
function shareOf(row: Row): number {
  try {
    return clamp(Number(BigInt(row.view.balance)) / Number(BigInt(row.record.denomination)));
  } catch {
    return 0;
  }
}

/** What the chosen note is: its balance, its life and where it pays out. */
function NoteSummary({ row, selected }: { row: Row; selected: boolean }) {
  const { record, view } = row;
  const leave = record.leave;
  const paid = leave?.state === 'closed' || leave?.state === 'exited';
  const daysLeft = Math.max(0, Math.ceil((view.expiry - Math.floor(Date.now() / 1000)) / 86_400));
  const mark: { status: NoteStatus; day: number } = {
    status: paid
      ? 'closed'
      : leave
        ? leave.state === 'closing'
          ? 'closing'
          : 'exiting'
        : daysLeft <= CLOSE_DAYS
          ? 'expiring'
          : 'open',
    day: 30 - daysLeft,
  };
  return (
    <li
      data-testid="leave-note"
      data-state={leave?.state ?? 'open'}
      className="nc-lv-sum"
      hidden={!selected}
    >
      <header className="nc-lv-head">
        <h3>Note #{view.noteId}</h3>
        <Chip tone={paid ? 'good' : 'plain'}>{leave ? STATE_TEXT[leave.state] : 'open'}</Chip>
        <p className="nc-lv-amount">
          <strong>{units(view.balance)}</strong>
          <small>of {units(record.denomination)}</small>
        </p>
      </header>
      <Strip
        n={20}
        solid={shareOf(row) * 20}
        size="md"
        label={`${units(view.balance)} of ${units(record.denomination)} left`}
      />
      <dl className="nc-lv-facts">
        <div>
          <dt>Expires</dt>
          <dd>
            {formatDate(view.expiry)}
            {!leave && <Status n={mark} />}
          </dd>
        </div>
        <div>
          <dt>Payout account</dt>
          <dd>
            <strong data-testid="leave-payout">{record.payout}</strong>
          </dd>
        </div>
        <div>
          <dt>State</dt>
          <dd>
            <span data-testid="leave-state">{leave ? STATE_TEXT[leave.state] : 'open'}</span>
          </dd>
        </div>
      </dl>
    </li>
  );
}

/** The way out of the chosen note: one choice at a time, one main action in it. */
function NoteWays({
  row,
  chainNow,
  prover,
  selected,
}: {
  row: Row;
  chainNow: number | null;
  prover: boolean;
  selected: boolean;
}) {
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();
  const [way, setWay] = useState<'close' | 'exit'>('close');
  const { record, view } = row;
  const leave = record.leave;
  const busy = s.busy !== null;
  const mine = s.busy?.id === record.id;
  const pending = s.pending[record.id];
  const connected = wallet.connected !== null;
  const canAct = prover && !busy;
  const leaving = !leave || leave.state === 'closing';
  const paid = leave?.state === 'closed' || leave?.state === 'exited';
  const hint = !connected
    ? 'Connect a wallet in the step above to sign.'
    : !prover
      ? 'Waiting for the proving key…'
      : busy
        ? 'Another step is running.'
        : '';
  // The exit waits 7 days (PUBLIC_SENTENCE): the cells count the days that have passed.
  const days =
    leave?.state === 'exiting' && leave.deadline !== undefined && chainNow !== null
      ? clamp(7 - Math.ceil((leave.deadline - chainNow) / 86_400), 0, 7)
      : 0;
  const left =
    leave?.deadline !== undefined && chainNow !== null ? leave.deadline - chainNow : null;

  const settle = async () => {
    // Best effort, like `nymirad`: an unsettled lease is finished if the operator can.
    const deps = await chatDeps(requireKey);
    const session = new ChatSession(deps, record);
    await session.resume({ pollMs: 500, maxWaitMs: 20_000 });
  };

  return (
    <div className="nc-lv-ways" hidden={!selected} data-state={leave?.state ?? 'open'}>
      {view.active?.token !== undefined && (
        <p className="nc-lv-note-text" data-testid="leave-lease-open">
          A lease is open on this note. End the conversation on the Chat screen before closing.
        </p>
      )}
      {view.pending && view.active?.token === undefined && !leave && (
        <p className="nc-lv-note-text" data-testid="leave-pending">
          An earlier lease is not settled yet. A close needs it settled first (Chat screen, “Settle
          the open lease”). An exit goes ahead and concedes the lease’s bound.
        </p>
      )}

      {leaving && (
        <>
          <div className="nc-lv-tabs" role="tablist" aria-label="Way out">
            <button
              type="button"
              role="tab"
              data-testid="way-close"
              aria-selected={way === 'close'}
              onClick={() => setWay('close')}
            >
              <strong>Close with the operator</strong>
              <small>now</small>
            </button>
            <button
              type="button"
              role="tab"
              data-testid="way-exit"
              aria-selected={way === 'exit'}
              onClick={() => setWay('exit')}
            >
              <strong>Exit without the operator</strong>
              <small>7 days</small>
            </button>
          </div>

          <div className="nc-lv-panel" role="tabpanel" hidden={way !== 'close'} data-way="close">
            <LeavePath
              kind="close"
              amount={units(view.balance)}
              payout={record.payout}
              active={leave?.state === 'closing'}
            />
            <p className="nc-lv-note-text" data-testid="operator-close-note">
              {OPERATOR_CLOSE_NOTE}
            </p>
            <div className="nc-lv-actions">
              <Button
                primary
                testId="close-wallet"
                disabled={!canAct || !connected}
                onClick={() => void leaveStore.close(record, 'wallet')}
              >
                <span>{leave?.state === 'closing' ? 'Continue the close' : 'Close note'}</span>
                <small>sign with your wallet, no close fee</small>
              </Button>
              <Button
                testId="close-operator"
                disabled={!canAct}
                onClick={() => void leaveStore.close(record, 'operator')}
              >
                <span>Close through the operator</span>
                <small>it pays the gas and takes the close fee</small>
              </Button>
              {leave?.state === 'closing' && (
                <Button
                  testId="close-recheck"
                  disabled={!canAct}
                  onClick={() => void leaveStore.recheckClosed(record)}
                >
                  Ask the vault whether the close landed
                </Button>
              )}
            </div>
            {hint && <p className="nc-lv-hint">{hint}</p>}
          </div>

          <div className="nc-lv-panel" role="tabpanel" hidden={way !== 'exit'} data-way="exit">
            <LeavePath kind="exit" amount={units(view.balance)} payout={record.payout} />
            <p className="nc-lv-note-text" data-testid="exit-note">
              An exit sends <code>request_exit</code>. It concedes{' '}
              <strong data-testid="exit-concession">{units(view.boundAtState)}</strong> to the
              operator: 0 when no request left this note’s latest state, the stored bound of that
              state otherwise. After the exit proof is made, this note makes no new request.
            </p>
            <div className="nc-lv-actions">
              <Button
                primary
                testId="exit-request"
                disabled={!canAct || !connected}
                onClick={() => void leaveStore.exit(record, view.pending ? settle : undefined)}
              >
                <span>Request exit</span>
                <small>the operator is not cooperating</small>
              </Button>
            </div>
            {hint && <p className="nc-lv-hint">{hint}</p>}
          </div>
        </>
      )}

      {leave?.state === 'exiting' && (
        <div className="nc-lv-panel nc-lv-exiting" data-testid="exit-panel">
          <div className="nc-lv-count" aria-live="polite">
            {left !== null && left > 0 ? (
              <strong>
                {Math.floor(left / 86_400)}
                <small>d</small> {Math.floor((left % 86_400) / 3600)}
                <small>h</small> {Math.floor((left % 3600) / 60)}
                <small>min</small>
              </strong>
            ) : (
              <strong>{left === null ? '…' : 'ready'}</strong>
            )}
            <span>
              {left !== null && left > 0 ? 'until finalize_exit works' : 'finalize_exit works now'}
            </span>
          </div>
          <ExitDays days={days} requested>
            {leave.deadline !== undefined ? (
              <p className="nc-lv-note-text">
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
              <p className="nc-lv-note-text" data-testid="exit-unconfirmed">
                The exit request was sent, but the vault does not list it yet.
              </p>
            )}
          </ExitDays>
          <div className="nc-lv-actions">
            {leave.deadline === undefined && (
              <Button
                testId="exit-recheck"
                disabled={busy}
                onClick={() => void leaveStore.refreshExitState(record)}
              >
                Ask the vault again
              </Button>
            )}
            <Button
              primary
              testId="exit-finalize"
              disabled={
                !canAct ||
                !connected ||
                leave.deadline === undefined ||
                chainNow === null ||
                chainNow < leave.deadline
              }
              onClick={() => void leaveStore.finalize(record)}
            >
              <span>Finalize exit</span>
              <small>
                {leave.deadline !== undefined && chainNow !== null && chainNow < leave.deadline
                  ? `available ${when(leave.deadline)}`
                  : 'pays your payout account'}
              </small>
            </Button>
          </div>
          {hint && <p className="nc-lv-hint">{hint}</p>}
        </div>
      )}

      {paid && (
        <div className="nc-lv-panel nc-lv-paid nc-done" data-testid="paid-panel">
          <div className="nc-lv-check" aria-hidden="true">
            <i />
          </div>
          <p>
            The vault pays <strong>{record.payout}</strong>. If that transfer could not be sent, the
            money waits in the vault until <code>claim_payout</code>.
          </p>
          <div className="nc-lv-actions nc-lv-actions-row">
            <Button
              testId="claim-check"
              disabled={busy}
              onClick={() => void leaveStore.readPending(record)}
            >
              Check for a pending payout
            </Button>
            <Button
              testId="claim-payout"
              disabled={!canAct || !connected}
              onClick={() => void leaveStore.claim(record)}
            >
              Claim payout
            </Button>
          </div>
          {pending !== undefined && (
            <p data-testid="claim-pending">
              {pending === '0'
                ? `Nothing is pending for ${record.payout}.`
                : `${units(pending)} is pending for ${record.payout}.`}
            </p>
          )}
          <p className="nc-lv-note-text" data-testid="claim-note">
            <Ticks text={CLAIM_NOTE} />
          </p>
        </div>
      )}
      {mine && s.busy && (
        <p className="nc-msg" data-tone="warn" data-testid="leave-busy" aria-live="polite">
          {s.busy.text}
        </p>
      )}
    </div>
  );
}

export function Close({ onFund }: { onFund?: () => void }) {
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();
  const [prover, setProver] = useState<ProverState>({ status: 'loading' });
  const [chainNow, setChainNow] = useState<number | null>(null);
  const [pick, setPick] = useState('');

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
  const current = rows.find((r) => r.record.id === pick) ?? rows[0];
  const stepOf = (i: number): 'done' | 'now' | 'next' =>
    rows.length === 0
      ? i === 0
        ? 'now'
        : 'next'
      : i === 0
        ? 'done'
        : i === 1
          ? wallet.connected
            ? 'done'
            : 'now'
          : 'now';

  return (
    <Screen
      testId="close-screen"
      title="Close or exit a note"
      lead="Closing returns the balance to the note’s payout account. If the operator will not sign, you can leave without it."
      illustration={closeIllustration}
    >
      {advice && (
        <p className="nc-notice" role="alert" data-testid="emergency-advice">
          {advice}
        </p>
      )}

      {rows.length === 0 && !s.loaded ? (
        <Card title="Notes">
          <Waiting flat testId="notes-loading" title="Reading your notes…">
            Opening the notes stored in this browser.
          </Waiting>
        </Card>
      ) : rows.length === 0 ? (
        <Card title="Notes">
          <Waiting tone="none" flat testId="no-open-notes" title="No funded note in this browser.">
            Close and exit work on a funded note. Fund one to use this screen.
          </Waiting>
          {onFund && (
            <div className="nc-actions">
              <Button primary onClick={onFund}>
                Fund a note
              </Button>
            </div>
          )}
        </Card>
      ) : (
        <ol className="nc-steps nc-lv-page">
          <li data-s={stepOf(0)}>
            <h2>Note</h2>
            {rows.length > 1 && (
              <div className="nc-lv-pick" role="group" aria-label="Choose a note">
                {rows.map((row) => {
                  const l = row.record.leave;
                  const done = l?.state === 'closed' || l?.state === 'exited';
                  return (
                    <button
                      key={row.record.id}
                      type="button"
                      aria-pressed={row.record.id === current?.record.id}
                      data-state={l?.state ?? 'open'}
                      onClick={() => setPick(row.record.id)}
                    >
                      <strong>#{row.view.noteId}</strong>
                      <span>{units(row.view.balance)}</span>
                      <small>{done ? 'paid out' : l ? STATE_TEXT[l.state] : 'open'}</small>
                    </button>
                  );
                })}
              </div>
            )}
            <ul className="nc-leave-notes">
              {rows.map((row) => (
                <NoteSummary
                  key={row.record.id}
                  row={row}
                  selected={row.record.id === current?.record.id}
                />
              ))}
            </ul>
          </li>
          <li data-s={stepOf(1)}>
            <h2>
              Wallet
              <Chip tone={prover.status === 'ready' ? 'good' : 'warn'}>
                {prover.status === 'ready' ? 'proving key ready' : 'proving key'}
              </Chip>
            </h2>
            <WalletConnect busy={s.busy !== null} />
            <p className="nc-lv-note-text" data-testid="withdrawal-prover-status">
              {prover.status === 'loading' && (
                <>
                  <WaitBar /> Loading the withdrawal proving key (SHA-256 checked)…
                </>
              )}
              {prover.status === 'ready' &&
                `Withdrawal proving key loaded and checked (${(prover.bytes / 1e6).toFixed(1)} MB, ${prover.ms} ms).`}
              {prover.status === 'off' && `${prover.reason} Close and exit are off in this build.`}
              {prover.status === 'failed' &&
                `The withdrawal proving key was refused: ${prover.reason}`}
            </p>
          </li>
          <li data-s={stepOf(2)}>
            <h2>Way out</h2>
            {rows.map((row) => (
              <NoteWays
                key={row.record.id}
                row={row}
                chainNow={chainNow}
                prover={prover.status === 'ready'}
                selected={row.record.id === current?.record.id}
              />
            ))}
            {s.message && (
              <p className="nc-msg" data-tone="ok" role="status" data-testid="leave-message">
                {s.message}
              </p>
            )}
            {s.error && (
              <p className="nc-msg" data-tone="err" role="alert" data-testid="leave-error">
                {s.error}
              </p>
            )}
          </li>
        </ol>
      )}
      {rows.length === 0 && s.message && (
        <p className="nc-msg" data-tone="ok" role="status" data-testid="leave-message">
          {s.message}
        </p>
      )}
      {rows.length === 0 && s.error && (
        <p className="nc-msg" data-tone="err" role="alert" data-testid="leave-error">
          {s.error}
        </p>
      )}
      <BackupAfterClose ids={s.backupFor} />

      <Fold title="What leaving shows and what it does not cover">
        <blockquote className="nc-quote" data-testid="public-sentence">
          <Ticks text={PUBLIC_SENTENCE} />
        </blockquote>
        <p className="nc-cap" data-testid="old-state-limit">
          {OLD_STATE_LIMIT}
        </p>
        <p className="nc-cap" data-testid="close-reveals">
          {CLOSE_REVEALS}
        </p>
        <p className="nc-cap" data-testid="gas-note">
          <Ticks text={GAS_NOTE} />
        </p>
      </Fold>

      <Fold
        title="Expiry guard"
        badge={
          <Chip tone={s.settings.enabled ? 'good' : 'warn'}>
            {s.settings.enabled ? 'on' : 'off'}
          </Chip>
        }
      >
        <p className="nc-cap nc-cap-first">
          A note that reaches its expiry can go to the treasury 7 days later, unless an exit is
          pending. While this app is open it guards the last days. Each step waits for your wallet
          signature.
        </p>
        <GuardTimeline off={!s.settings.enabled} />
        <div className="nc-guard-switches">
          <label className="nc-keep">
            <input
              type="checkbox"
              data-testid="guard-enabled"
              checked={s.settings.enabled}
              onChange={(e) =>
                void leaveStore.setSettings({ ...s.settings, enabled: e.target.checked })
              }
            />
            <span className="nc-keep-switch" aria-hidden="true" />
            <span className="nc-keep-text">
              <strong>Guard my notes near expiry</strong>
              <small>On by default.</small>
            </span>
          </label>
          <label className="nc-keep">
            <input
              type="checkbox"
              data-testid="close-at-end"
              checked={s.settings.closeAtSessionEnd}
              onChange={(e) =>
                void leaveStore.setSettings({ ...s.settings, closeAtSessionEnd: e.target.checked })
              }
            />
            <span className="nc-keep-switch" aria-hidden="true" />
            <span className="nc-keep-text">
              <strong>Start a close when a conversation ends</strong>
              <small>
                Off by default: every close shows the note ID and the total spend on chain.
              </small>
            </span>
          </label>
        </div>
      </Fold>
    </Screen>
  );
}
