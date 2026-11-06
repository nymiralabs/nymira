// Funding from another chain, direct path: the user picks a source token, the app makes and backs
// up the note secret, asks 1Click for a signed quote, shows what the payment costs and who sees
// what, and only then shows the deposit address. The user pays from their own wallet on the source
// chain. Progress comes from 1Click; the note is accepted from the vault's `deposit` event.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { PROFILE } from '../profile';
import { accountFingerprint, assessPayout, type PayoutAssessment } from '../chain/token';
import type { VaultCheck } from '../chain/vault';
import { expiryCalendar } from '../backup/ics';
import { backupFileName, BACKUP_EXTENSION } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import {
  loadNotes,
  makeBackupFile,
  notePrice,
  prepareNote,
  testRestore,
  update,
} from '../funding/flow';
import {
  INTENTS_SIGNER,
  KEYLESS_FEE_BPS,
  PRIVACY,
  ROUTE_FALLBACK,
  TRUST_NOTE,
  beforeAddress,
  confirmIntents,
  explainBooking,
  fetchStatus,
  loadAttempt,
  payoutProblem,
  requestQuote,
  saveAttempt,
  statusText,
  type Attempt,
  type IntentsResult,
  type Json,
} from '../funding/intents';
import { leaveStore } from '../leave/store';
import { QuoteBook, type QuoteOutcome } from '../funding/oneclick';
import type { QuoteAsset } from '../profiles/types';
import type { NoteRecord } from '../store/types';
import { download, formatDate, formatUnits, readFile } from './format';
import { archivalRpc, rpc, useWallet } from './services';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;
/** The keyless limit is one request per 5 seconds; the status poll stays under it. */
const POLL_MS = 5000;

export function Intents({
  vault,
  denomination,
  unlocked,
}: {
  vault: VaultCheck | null;
  denomination: string;
  unlocked: boolean;
}) {
  const pins = PROFILE.pins;
  const book = useMemo(
    () => new QuoteBook(PROFILE.oneClick.baseUrl, PROFILE.oneClick.signerKey),
    [],
  );
  const [rows, setRows] = useState<QuoteOutcome[] | null>(null);
  const [source, setSource] = useState<QuoteAsset | null>(null);
  const [refund, setRefund] = useState('');
  const [payout, setPayout] = useState('');
  const [fingerprint, setFingerprint] = useState('');
  const [confirmed, setConfirmed] = useState(false);
  const [assessment, setAssessment] = useState<PayoutAssessment | 'checking' | 'failed' | null>(
    null,
  );
  const [note, setNote] = useState<NoteRecord | null>(null);
  const [backupPass, setBackupPass] = useState('');
  const [downloaded, setDownloaded] = useState(false);
  const [attempt, setAttempt] = useState<Attempt | null>(null);
  const [shown, setShown] = useState(false);
  const [expired, setExpired] = useState(false);
  const [progress, setProgress] = useState('');
  const [accepted, setAccepted] = useState<Extract<IntentsResult, { status: 'accepted' }> | null>(
    null,
  );
  const [booked, setBooked] = useState<Extract<IntentsResult, { status: 'booked' }> | null>(null);
  /** The payout account's total in the vault's `pending_payout`, base units, when it was read. */
  const [waiting, setWaiting] = useState<string | undefined>();
  const [pendingReason, setPendingReason] = useState('');
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const noteRef = useRef<NoteRecord | null>(null);
  noteRef.current = note;
  const wallet = useWallet();
  const leave = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);

  const vaultOk = vault?.status === 'ok';
  const problem = payout === '' ? null : payoutProblem(payout);
  const validPayout = payout !== '' && problem === null;
  const price = pins ? notePrice(note?.denomination ?? denomination) : 0n;
  const stage = note?.phase ?? 'none';

  useEffect(() => {
    setConfirmed(false);
    setAssessment(null);
    if (!validPayout) {
      setFingerprint('');
      return;
    }
    let live = true;
    accountFingerprint(payout).then((f) => live && setFingerprint(f));
    setAssessment('checking');
    assessPayout(rpc, PROFILE.tokenAccount, payout, '').then(
      (a) => live && setAssessment(a),
      () => live && setAssessment('failed'),
    );
    return () => {
      live = false;
    };
  }, [payout, validPayout]);

  // Status polling, one request per tick; the destination transaction is read once it is named.
  useEffect(() => {
    if (!shown || !attempt || accepted || booked) return;
    let live = true;
    let hash = attempt.hash ?? '';
    let amount = attempt.amount;
    const deadline = Date.parse(String((attempt.quote.quote as Json).deadline));
    const tick = async () => {
      try {
        if (hash === '') {
          const result = await fetchStatus(attempt);
          if (!live) return;
          if (result.state !== 'success')
            return setProgress(statusText(result, deadline, Date.now()));
          hash = result.hash;
          amount = result.amount;
          setProgress('1Click reports SUCCESS. Reading the deposit receipt.');
          // Kept before the receipt is read, so a closed tab resumes from the destination hash.
          await saveAttempt({ ...attempt, hash, ...(amount ? { amount } : {}) });
          const current = noteRef.current;
          if (current) setNote(await update(current, { phase: 'sent', txHash: hash }));
        }
        const current = noteRef.current;
        if (!current) return;
        const result = await confirmIntents(rpc, current, hash, {
          archival: archivalRpc,
          ...(amount ? { delivered: amount } : {}),
        });
        if (!live) return;
        setWaiting(result.pending);
        if (result.status === 'accepted') {
          setNote(result.record);
          setAccepted(result);
        } else if (result.status === 'booked') setBooked(result);
        else setPendingReason(result.reason);
      } catch (e) {
        if (live) setProgress(e instanceof Error ? e.message : String(e));
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [shown, attempt, accepted, booked]);

  // A closed tab or a reload: the newest unfinished Intents note with a stored quote picks up from
  // its phase. The quote record is sealed, so this waits for the passphrase.
  useEffect(() => {
    if (!unlocked || !pins) return;
    let live = true;
    void (async () => {
      const open = (await loadNotes()).filter(
        (n) => n.sender === INTENTS_SIGNER && n.kind === 'pending' && n.phase !== 'rejected',
      );
      for (const n of open) {
        const saved = await loadAttempt(n.id).catch(() => null);
        if (!saved) continue;
        if (!live || noteRef.current) return;
        setNote(n);
        setAttempt(saved);
        setRefund(saved.refund);
        setSource(PROFILE.oneClick.shortlist.find((a) => a.label === saved.source) ?? null);
        setShown(n.phase === 'sending' || n.phase === 'sent');
        return;
      }
    })();
    return () => {
      live = false;
    };
  }, [unlocked, pins]);

  const act = async (label: string, action: () => Promise<void>) => {
    setBusy(label);
    setError('');
    try {
      await action();
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setExpired(message.startsWith('the quote expired'));
      setError(message);
    } finally {
      setBusy('');
    }
  };

  const loadSources = () =>
    act('sources', async () => {
      setRows([]);
      setSource(null);
      for (const asset of PROFILE.oneClick.shortlist) {
        const outcome = await book.quote(asset, price);
        setRows((previous) => [...(previous ?? []), outcome]);
      }
    });

  const onPrepare = () =>
    act('prepare', async () => {
      const record = await prepareNote({ denomination, payout, sender: INTENTS_SIGNER });
      setNote(record);
      setDownloaded(false);
      await navigator.storage?.persist?.();
    });

  const onDownload = () =>
    act('download', async () => {
      const bytes = await makeBackupFile(backupPass);
      download(bytes as BlobPart, backupFileName(), 'application/octet-stream');
      setDownloaded(true);
    });

  const onChooseBackup = (file: File | undefined) => {
    if (!file || !note) return;
    act('test', async () => setNote(await testRestore(note, await readFile(file), backupPass)));
  };

  const onQuote = () =>
    act('quote', async () => {
      if (!note || !source) return;
      setExpired(false);
      setShown(false);
      setAttempt(null);
      setAttempt(await requestQuote(rpc, note, source, refund));
    });

  const onShowAddress = () =>
    act('address', async () => {
      if (!attempt || !note) return;
      await beforeAddress(rpc, attempt);
      // Stored before the address is shown, so a closed tab leaves a note that can be looked up.
      setNote(await update(note, { phase: 'sending' }));
      setShown(true);
    });

  const q = (attempt?.quote.quote ?? {}) as Json;
  const fees = [
    ['refundFee', q.refundFee],
    ['withdrawFee', q.withdrawFee],
    ['appFees', (attempt?.quote.quoteRequest as Json | undefined)?.appFees],
  ].filter(([, v]) => v);

  const ready =
    unlocked &&
    vaultOk &&
    source !== null &&
    refund.trim() !== '' &&
    validPayout &&
    confirmed &&
    typeof assessment === 'object' &&
    assessment !== null;

  if (!pins) return null;
  const listed = (rows ?? []).filter(
    (r): r is Extract<QuoteOutcome, { quote: unknown }> => 'quote' in r,
  );

  return (
    <section data-testid="intents">
      <h3>Fund from another chain</h3>
      <p className="hint" data-testid="intents-tor">
        If you use Tor Browser: 1Click calls on this page must not go through Tor, because 1Click
        does not accept Tor traffic. Use a normal browser for this step, or fund with{' '}
        {PROFILE.tokenSymbol} on NEAR above.
      </p>
      <p className="warning" data-testid="intents-privacy">
        {PRIVACY}
      </p>
      <details data-testid="intents-terms">
        <summary>Terms for this path</summary>
        <p>
          You pay 1Click from your own wallet. 1Click and its solvers hold the funds while they are
          in transit and decide the exchange rate within the quote you accept. Nymira does not
          operate 1Click, cannot refund a failed order, and receives nothing from it. Quotes are
          kept on this device only. A refunded payment goes to the refund address you enter, on the
          source chain. Read the terms of use linked at the top of this page as well.
        </p>
      </details>
      <p className="hint">
        The quote and status requests go from this page to 1Click, which sees your IP address (and,
        once you ask for a real quote, your refund address, the note’s commitment and the payout
        account).
      </p>

      <h4>Source</h4>
      <button
        type="button"
        data-testid="intents-load-sources"
        disabled={busy !== '' || !vaultOk || stage !== 'none'}
        onClick={loadSources}
      >
        {busy === 'sources' ? 'Asking 1Click…' : 'Show sources'}
      </button>
      {rows && (
        <ul>
          {listed.map(({ quote }) => {
            const asset = PROFILE.oneClick.shortlist.find((a) => a.label === quote.label);
            return asset ? (
              <li key={asset.assetId}>
                <label>
                  <input
                    type="radio"
                    name="intents-source"
                    data-testid="intents-source"
                    disabled={stage !== 'none'}
                    checked={source?.assetId === asset.assetId}
                    onChange={() => setSource(asset)}
                  />
                  {asset.label}: about {quote.amountInFormatted} in
                </label>
              </li>
            ) : null;
          })}
        </ul>
      )}
      {rows && busy !== 'sources' && listed.length === 0 && (
        <p className="warning" data-testid="intents-no-sources">
          1Click offers no source for this amount right now.
        </p>
      )}
      <label>
        Refund address on the source chain
        <input
          type="text"
          value={refund}
          data-testid="intents-refund"
          disabled={stage !== 'none'}
          onChange={(e) => setRefund(e.target.value)}
        />
      </label>

      <h4>Payout account</h4>
      <p>
        The vault pays this NEAR account when you close the note or exit. It is written into the
        deposit message and cannot be changed afterwards. Use a fresh account that nothing else
        links to you.
      </p>
      <input
        type="text"
        aria-label="Payout account"
        value={payout}
        data-testid="intents-payout"
        disabled={stage !== 'none'}
        onChange={(e) => setPayout(e.target.value.trim())}
      />
      {problem && (
        <p className="error" data-testid="intents-payout-problem">
          {problem}
        </p>
      )}
      {validPayout && (
        <>
          <p>
            Fingerprint of <strong>{payout}</strong>:{' '}
            <code data-testid="intents-fingerprint">{fingerprint}</code>. Check the account letter
            by letter.
          </p>
          <label>
            <input
              type="checkbox"
              checked={confirmed}
              data-testid="intents-fingerprint-confirm"
              onChange={(e) => setConfirmed(e.target.checked)}
            />
            This is the account I want paid.
          </label>
          <p className="hint">
            Checking this account asks the RPC provider about it, so the provider sees it next to
            your IP address.
          </p>
          {assessment === 'failed' && (
            <p className="warning" data-testid="intents-assess-failed">
              The account could not be checked.
            </p>
          )}
          {typeof assessment === 'object' && assessment !== null && (
            <>
              {!assessment.registered && (
                <p className="warning" data-testid="intents-warn-unregistered">
                  This account is not registered with the {PROFILE.tokenSymbol} token. A payout to
                  it would wait until it is registered (call <code>storage_deposit</code> for it).
                </p>
              )}
              {assessment.registered && !assessment.fresh && (
                <p className="warning" data-testid="intents-warn-not-fresh">
                  This account already holds {PROFILE.tokenSymbol}, so it is not fresh. A payout to
                  it is easy to link to its history.
                </p>
              )}
            </>
          )}
        </>
      )}
      <p className="hint" data-testid="intents-costs">
        Closing a note through the operator costs the close fee ({units(pins.config.close_fee)}). An
        exit or a payout claim needs a little NEAR for gas, which can come from any account.
      </p>

      <h4>Protect and back up</h4>
      {!unlocked ? (
        <p className="hint">Set or enter your passphrase in step 4 above first.</p>
      ) : stage === 'none' || stage === 'rejected' ? (
        <button
          type="button"
          className="primary"
          data-testid="intents-prepare"
          disabled={!ready || busy !== ''}
          onClick={onPrepare}
        >
          {busy === 'prepare' ? 'Creating the note…' : 'Create the note secret'}
        </button>
      ) : (
        <div data-testid="intents-backup-panel">
          <label>
            Backup passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)
            <input
              type="password"
              autoComplete="new-password"
              value={backupPass}
              data-testid="intents-backup-passphrase"
              onChange={(e) => setBackupPass(e.target.value)}
            />
          </label>
          <button
            type="button"
            data-testid="intents-backup-download"
            disabled={
              passphraseLength(backupPass) < MIN_PASSPHRASE_LENGTH ||
              busy !== '' ||
              stage !== 'prepared'
            }
            onClick={onDownload}
          >
            {busy === 'download' ? 'Encrypting…' : `Download the backup (${BACKUP_EXTENSION})`}
          </button>
          {stage === 'prepared' && downloaded && (
            <label>
              Now choose the file you just downloaded, to test that it opens
              <input
                type="file"
                data-testid="intents-backup-file"
                disabled={busy !== ''}
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  e.target.value = '';
                  onChooseBackup(file);
                }}
              />
            </label>
          )}
          {stage !== 'prepared' && (
            <p className="ok" data-testid="intents-backup-tested">
              Backup downloaded and test-restored. The note secret is stored on this device.
            </p>
          )}
        </div>
      )}

      <h4>Quote</h4>
      <button
        type="button"
        className="primary"
        data-testid="intents-quote"
        disabled={stage !== 'backed_up' || shown || busy !== '' || (attempt !== null && !expired)}
        onClick={onQuote}
      >
        {busy === 'quote' ? 'Asking 1Click…' : 'Get the quote'}
      </button>
      {error && (
        <p className="error" role="alert" data-testid="intents-error">
          {error}
        </p>
      )}
      {expired && (
        <button
          type="button"
          data-testid="intents-requote"
          disabled={busy !== ''}
          onClick={onQuote}
        >
          Get a new quote
        </button>
      )}
      {attempt && !expired && (
        <div data-testid="intents-review">
          <p>
            Deadline: <strong data-testid="intents-deadline">{String(q.deadline)}</strong>
          </p>
          <p data-testid="intents-cost">
            You pay about <strong>{String(q.amountInFormatted)}</strong> ({attempt.source}).{' '}
            <strong>{String(q.amountOutFormatted)}</strong> {PROFILE.tokenSymbol} is delivered (
            {units(price)}: the denomination plus the note fee). The difference is the all-in cost.
            It includes 1Click’s documented fee for keyless use, {KEYLESS_FEE_BPS} basis points of
            the amount delivered; the quote does not itemise it, so no split is shown.
          </p>
          <p data-testid="intents-fees">
            {fees.length === 0
              ? 'The quote lists no other fees.'
              : `Other fees the quote lists: ${fees.map(([n, v]) => `${n}=${JSON.stringify(v)}`).join(', ')}.`}
          </p>
          <p>
            Refund address: <code data-testid="intents-refund-shown">{attempt.refund}</code>
          </p>
          <p className="hint" data-testid="intents-trust">
            {TRUST_NOTE}
          </p>
          {!shown && (
            <button
              type="button"
              className="primary"
              data-testid="intents-show-address"
              disabled={busy !== ''}
              onClick={onShowAddress}
            >
              Show the deposit address
            </button>
          )}
        </div>
      )}

      {shown && attempt && (
        <div data-testid="intents-address-panel">
          <p>
            Pay from your own wallet on the source chain. Send exactly{' '}
            <strong>{String(q.amountInFormatted)}</strong> to:
          </p>
          <p>
            <code data-testid="intents-address">{String(q.depositAddress)}</code>
            {typeof q.depositMemo === 'string' && q.depositMemo !== '' && (
              <>
                {' '}
                with memo <code data-testid="intents-memo">{q.depositMemo}</code>
              </>
            )}
          </p>
          <p data-testid="intents-progress">{progress || 'Waiting for your payment.'}</p>
          {pendingReason && !accepted && !booked && (
            <p className="warning" data-testid="intents-pending">
              The deposit is not accepted yet: {pendingReason}.
            </p>
          )}
          <p className="hint" data-testid="intents-route">
            {ROUTE_FALLBACK}
          </p>
        </div>
      )}

      {waiting !== undefined && note && (
        <p className="hint" data-testid="intents-pending-payout">
          {waiting === '0'
            ? `Nothing is pending in the vault for ${note.payout}.`
            : `${units(waiting)} is pending in the vault for ${note.payout}. This is an account total across deposits and does not confirm a note.`}{' '}
          Reading it showed the payout account to the RPC provider, next to your IP address.
        </p>
      )}

      {(booked ?? accepted)?.bookings.length ? (
        <div className="warning" data-testid="intents-booked">
          <h4>
            {booked ? 'The vault booked this delivery without a note' : 'The vault also booked'}
          </h4>
          {(booked ?? accepted)?.bookings.map((b) => (
            <p key={`${b.event}${b.reason}${b.amount}`} data-testid="intents-booking">
              {explainBooking(b)}
            </p>
          ))}
          {(booked ?? accepted)?.bookings.some((b) => b.event === 'payout_pending') && note && (
            <p>
              <button
                type="button"
                data-testid="intents-claim"
                disabled={!wallet.connected || leave.busy !== null}
                onClick={() => void leaveStore.claim(note)}
              >
                Claim payout
              </button>{' '}
              {!wallet.connected && 'Connect a wallet above to send it. '}
              {leave.message}
              {leave.error && <span className="error"> {leave.error}</span>}
            </p>
          )}
        </div>
      ) : null}

      {accepted && note && (
        <div className="ok" data-testid="intents-accepted">
          <h4>Deposit accepted</h4>
          <p>
            Note <strong data-testid="intents-note-id">#{accepted.event.noteId}</strong>, expires{' '}
            <strong>{formatDate(accepted.event.expiry)}</strong>. The deposit event was read from
            the destination transaction’s receipts.
          </p>
          {note.txHash && (
            <p>
              <a
                data-testid="intents-explorer-link"
                href={PROFILE.explorerTxUrl(note.txHash)}
                target="_blank"
                rel="noopener noreferrer"
              >
                Open the deposit receipt in the explorer
              </a>
              . On this path no wallet shows the payout account, so read it there: it must be{' '}
              <strong>{note.payout}</strong>.
            </p>
          )}
          <button
            type="button"
            data-testid="intents-calendar"
            onClick={() =>
              download(
                expiryCalendar(accepted.event.expiry, crypto.randomUUID(), Date.now()),
                'note-expiry.ics',
                'text/calendar',
              )
            }
          >
            Download a calendar reminder for the expiry
          </button>
          <p className="hint">
            The reminder has a generic title and no note data. Download a fresh backup from the
            Notes tab: it now holds the note’s wallet state. Wait a while before your first chat.
          </p>
        </div>
      )}
    </section>
  );
}
