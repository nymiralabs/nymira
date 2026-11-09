// Funding from another chain, direct path: the user picks a source token, the app makes and backs
// up the note secret, asks 1Click for a signed quote, shows what the payment costs and who sees
// what, and only then shows the deposit address. The user pays from their own wallet on the source
// chain. Progress comes from 1Click; the note is accepted from the vault's `deposit` event.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { BackupSteps, Button, Chip, Facts, Fold } from '../marketing/kit';
import { Icon } from '../marketing/Icon';
import { PROFILE } from '../profile';
import {
  accountFingerprint,
  assessPayout,
  tokenBalance,
  type PayoutAssessment,
} from '../chain/token';
import type { VaultCheck } from '../chain/vault';
import { expiryCalendar } from '../backup/ics';
import { backupFileName, BACKUP_EXTENSION } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import {
  confirmDeposit,
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
  PRIVACY_TWO_STEP,
  ROUTE_FALLBACK,
  TRUST_NOTE,
  TWO_STEP_NOTICE,
  beforeAddress,
  confirmIntents,
  explainBooking,
  fetchStatus,
  loadAttempt,
  payoutProblem,
  requestQuote,
  saveAttempt,
  sendToVault,
  statusText,
  twoStepProblem,
  type Attempt,
  type IntentsResult,
  type Json,
} from '../funding/intents';
import { leaveStore } from '../leave/store';
import { QuoteBook, intentsPaths, type QuoteOutcome } from '../funding/oneclick';
import type { QuoteAsset } from '../profiles/types';
import type { NoteRecord } from '../store/types';
import { download, formatDate, formatUnits, readFile } from './format';
import { archivalRpc, rpc, useWallet } from './services';
import { Unlock } from './Unlock';

/** The quote's deadline as the person's local time; an unreadable value is shown as it came. */
function deadlineText(value: unknown): string {
  const at = Date.parse(String(value));
  return Number.isNaN(at)
    ? String(value)
    : new Date(at).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
}

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;
/** The keyless limit is one request per 5 seconds; the status poll stays under it. */
const POLL_MS = 5000;

export function Intents({
  vault,
  denomination,
  unlocked,
  active,
  onUnlocked,
}: {
  vault: VaultCheck | null;
  denomination: string;
  unlocked: boolean;
  /** This route is the one on screen: only then does it show the passphrase card. */
  active: boolean;
  onUnlocked: () => void;
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
  const [copied, setCopied] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState('');
  const noteRef = useRef<NoteRecord | null>(null);
  noteRef.current = note;
  const wallet = useWallet();
  const leave = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  // Two-step path: 1Click pays the connected account, which signs the transfer to the vault.
  const offered = intentsPaths(PROFILE);
  const [twoStep, setTwoStep] = useState(!offered.includes('direct'));
  const [gate, setGate] = useState<{ account: string; problem: string | null } | null>(null);
  const [arrived, setArrived] = useState(false);
  const [refunded, setRefunded] = useState(false);
  const account = wallet.connected?.accountId ?? '';
  const twoStepOk = !twoStep || (gate?.account === account && gate.problem === null);

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
    if (!shown || !attempt || attempt.via || accepted || booked) return;
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

  // Two-step path: the gate (NEAR for gas, USDC registrations) for the connected account.
  useEffect(() => {
    if (!twoStep || account === '') return;
    let live = true;
    setGate(null);
    twoStepProblem(rpc, account).then(
      (problem) => live && setGate({ account, problem }),
      () => live && setGate({ account, problem: 'The account could not be checked.' }),
    );
    return () => {
      live = false;
    };
  }, [twoStep, account]);

  // Two-step path, first step: wait for 1Click to deliver, then for the USDC in the account.
  useEffect(() => {
    if (!shown || !attempt?.via || accepted || arrived || note?.phase === 'sent') return;
    const via = attempt.via;
    let live = true;
    let hash = attempt.hash ?? '';
    const deadline = Date.parse(String((attempt.quote.quote as Json).deadline));
    const tick = async () => {
      try {
        if (hash === '') {
          const result = await fetchStatus(attempt);
          if (!live) return;
          if (result.state !== 'success')
            return setProgress(statusText(result, deadline, Date.now()));
          hash = result.hash;
          await saveAttempt({ ...attempt, hash });
        }
        const have = await tokenBalance(rpc, PROFILE.tokenAccount, via);
        if (!live) return;
        if (have >= BigInt(attempt.request.amount as string)) {
          setProgress(`The ${PROFILE.tokenSymbol} reached ${via}.`);
          setArrived(true);
        } else {
          setProgress(
            `1Click reports SUCCESS. Waiting for the ${PROFILE.tokenSymbol} to arrive in ${via}.`,
          );
        }
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
  }, [shown, attempt, accepted, arrived, note?.phase]);

  /** Acceptance from the second transaction's own receipts; a refused transfer can be retried. */
  const settle = async (record: NoteRecord, outcome: unknown) => {
    const result = await confirmDeposit(rpc, record, outcome, {
      onRpcRead: () => setProgress('Reading the transaction from the RPC provider.'),
    });
    if (result.status === 'accepted') {
      setNote(result.record);
      setAccepted({ ...result, bookings: [] });
    } else if (result.status === 'rejected') {
      if (result.reason.startsWith('the vault did not accept')) {
        // The token returned the USDC: the note is still unspent, so the deposit can be repeated.
        setRefunded(true);
        setNote(await update(result.record, { phase: 'sending' }));
      } else {
        setNote(result.record);
        setError(result.reason);
      }
    } else setPendingReason(result.reason);
  };

  // A closed tab or a reload: the newest unfinished Intents note with a stored quote picks up from
  // its phase. The quote record is sealed, so this waits for the passphrase.
  useEffect(() => {
    if (!unlocked || !pins) return;
    let live = true;
    void (async () => {
      const open = (await loadNotes()).filter(
        (n) => n.kind === 'pending' && n.phase !== 'rejected',
      );
      for (const n of open) {
        const saved = await loadAttempt(n.id).catch(() => null);
        // A saved attempt on a path this build does not offer is left alone, not resumed.
        if (!saved || !offered.includes(saved.via ? 'two-step' : 'direct')) continue;
        if (!live || noteRef.current) return;
        setNote(n);
        setAttempt(saved);
        setRefund(saved.refund);
        setPayout(n.payout);
        setSource(PROFILE.oneClick.shortlist.find((a) => a.label === saved.source) ?? null);
        setShown(n.phase === 'sending' || n.phase === 'sent');
        if (saved.via) {
          setTwoStep(true);
          if (n.phase === 'sent') void settle(n, null).catch((e) => setError(String(e)));
        }
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

  /** Drops the funding in progress: the note is marked rejected and the form starts again. */
  const cancelFunding = () =>
    act('cancel', async () => {
      if (!note) return;
      await update(note, { phase: 'rejected' });
      setNote(null);
      setAttempt(null);
      setShown(false);
      setExpired(false);
      setProgress('');
      setPendingReason('');
      setDownloaded(false);
      setBackupPass('');
      setPayout('');
      setConfirmed(false);
      setAssessment(null);
      setRefunded(false);
      setArrived(false);
      setCancelling(false);
    });

  const canLoad =
    busy === '' && vaultOk && stage === 'none' && twoStepOk && !(twoStep && account === '');

  const onPrepare = () =>
    act('prepare', async () => {
      const record = await prepareNote({
        denomination,
        payout,
        sender: twoStep ? account : INTENTS_SIGNER,
      });
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
      setAttempt(
        await requestQuote(
          rpc,
          note,
          source,
          refund,
          undefined,
          undefined,
          twoStep ? account : undefined,
        ),
      );
    });

  const onShowAddress = () =>
    act('address', async () => {
      if (!attempt || !note) return;
      await beforeAddress(rpc, attempt);
      // Stored before the address is shown, so a closed tab leaves a note that can be looked up.
      setNote(await update(note, { phase: 'sending' }));
      setShown(true);
    });

  const onSign = () =>
    act('sign', async () => {
      if (!note || !attempt?.via) return;
      if (account !== attempt.via) throw new Error(`connect ${attempt.via} to sign the transfer`);
      setRefunded(false);
      setPendingReason('');
      const sent = await sendToVault(rpc, note, wallet);
      setNote(sent.record);
      await settle(sent.record, sent.outcome);
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
    twoStepOk &&
    (!twoStep || account !== '') &&
    confirmed &&
    typeof assessment === 'object' &&
    assessment !== null;

  if (!pins) return null;
  const listed = (rows ?? []).filter(
    (r): r is Extract<QuoteOutcome, { quote: unknown }> => 'quote' in r,
  );

  const created = stage !== 'none' && stage !== 'rejected';
  const backedUp = created && stage !== 'prepared';
  const flags = [
    source !== null && refund.trim() !== '',
    created || (validPayout && confirmed && typeof assessment === 'object' && assessment !== null),
    backedUp,
    attempt !== null && !expired,
    accepted !== null,
  ];
  const now = flags.findIndex((f) => !f);
  const stepState = (i: number) => (flags[i] ? 'done' : i === now ? 'now' : 'next');

  return (
    <section className="nc-intents" data-testid="intents">
      {offered.length > 1 && (
        <div className="nc-intents-way">
          <div
            className="nc-seg nc-intents-path"
            role="radiogroup"
            aria-label="How the vault is paid"
            data-testid="intents-path"
          >
            <label>
              <input
                type="radio"
                name="intents-path"
                data-testid="intents-path-direct"
                checked={!twoStep}
                disabled={stage !== 'none'}
                onChange={() => setTwoStep(false)}
              />
              <span>Direct</span>
            </label>
            <label>
              <input
                type="radio"
                name="intents-path"
                data-testid="intents-path-two-step"
                checked={twoStep}
                disabled={stage !== 'none'}
                onChange={() => setTwoStep(true)}
              />
              <span>Two steps</span>
            </label>
          </div>
          <p>
            {twoStep
              ? '1Click pays your NEAR account, then you sign the transfer to the vault.'
              : '1Click pays the vault.'}
          </p>
        </div>
      )}
      <p className="nc-intents-line">
        {PRIVACY.split('. ')[0]}.{twoStep ? ` ${PRIVACY_TWO_STEP}` : ''}
      </p>
      <Fold title="Terms for this path" testId="intents-terms">
        <dl className="nc-terms-list">
          <div>
            <dt>Who sees this payment</dt>
            <dd data-testid="intents-privacy">
              {twoStep ? `${PRIVACY} ${PRIVACY_TWO_STEP}` : PRIVACY}
            </dd>
          </div>
          {twoStep && (
            <div>
              <dt>Why two steps</dt>
              <dd data-testid="intents-two-step-notice">{TWO_STEP_NOTICE}</dd>
            </div>
          )}
          <div>
            <dt>Who holds the money</dt>
            <dd>
              You pay 1Click from your own wallet. 1Click and its solvers hold the funds while they
              are in transit and decide the exchange rate within the quote you accept. Nymira does
              not operate 1Click, cannot refund a failed order, and receives nothing from it.
            </dd>
          </div>
          <div>
            <dt>Quotes and refunds</dt>
            <dd>
              Quotes are kept on this device only. A refunded payment goes to the refund address you
              enter, on the source chain. Read the terms of use linked at the top of this page as
              well.
            </dd>
          </div>
          <div>
            <dt>What the requests show</dt>
            <dd>
              The quote and status requests go from this page to 1Click, which sees your IP address
              (and, once you ask for a real quote, your refund address, the note’s commitment and
              the payout account).
              {twoStep
                ? ' Checking the account shows it to the RPC provider, next to your IP address.'
                : ''}
            </dd>
          </div>
          <div>
            <dt>Tor Browser</dt>
            <dd data-testid="intents-tor">
              If you use Tor Browser: 1Click calls on this page must not go through Tor, because
              1Click does not accept Tor traffic. Use a normal browser for this step, or fund with{' '}
              {PROFILE.tokenSymbol} on NEAR instead.
            </dd>
          </div>
        </dl>
      </Fold>
      {twoStep && (account === '' || (gate?.account === account && gate.problem)) && (
        <div data-testid="intents-two-step">
          {account === '' && (
            <p className="nc-intents-line">Connect a wallet (top right) to use this path.</p>
          )}
          {gate?.account === account && gate.problem && (
            <p className="nc-msg" data-tone="err" data-testid="intents-two-step-problem">
              {gate.problem}
            </p>
          )}
        </div>
      )}

      <ol className="nc-steps">
        <li data-s={stepState(0)}>
          <h2>Source</h2>
          <p className="nc-cap nc-cap-first">Pick the chain and token you pay from.</p>
          {rows === null && source === null && (
            <div className="nc-src-ask">
              <Button
                testId="intents-load-sources"
                disabled={!canLoad || busy !== ''}
                onClick={loadSources}
              >
                {busy === 'sources' ? 'Asking 1Click…' : 'Show sources'}
              </Button>
              <p>
                {twoStep && account === ''
                  ? 'Connect a wallet (top right) first.'
                  : 'Asks 1Click for prices. 1Click sees your IP address.'}
              </p>
            </div>
          )}
          {rows !== null && (
            <div className="nc-src-head">
              <span>{busy === 'sources' ? 'Asking 1Click…' : 'Prices from 1Click'}</span>
              <button
                type="button"
                className="nc-src-refresh"
                data-testid="intents-refresh-sources"
                data-busy={busy === 'sources'}
                aria-label="Refresh prices (asks 1Click again)"
                title="Refresh prices (asks 1Click again)"
                disabled={!canLoad}
                onClick={loadSources}
              >
                <Icon name="refresh" size={14} />
              </button>
            </div>
          )}
          {rows === null && source !== null && (
            <div className="nc-src-list">
              <div className="nc-src nc-src-static">
                <span
                  className="nc-chain"
                  data-chain={(source.label.split(' on ')[1] ?? '').toLowerCase()}
                  aria-hidden="true"
                />
                <span className="nc-src-name">{source.label.split(' on ')[1] ?? source.label}</span>
                <span className="nc-src-price">{source.label.split(' on ')[0]}</span>
              </div>
            </div>
          )}
          {rows && (
            <div className="nc-src-list" role="radiogroup" aria-label="Source">
              {listed.map(({ quote }) => {
                const asset = PROFILE.oneClick.shortlist.find((a) => a.label === quote.label);
                if (!asset) return null;
                const [token = '', chain = asset.label] = asset.label.split(' on ');
                const price = Number.parseFloat(String(quote.amountInFormatted));
                return (
                  <label key={asset.assetId} className="nc-src" aria-label={asset.label}>
                    <input
                      type="radio"
                      name="intents-source"
                      data-testid="intents-source"
                      disabled={stage !== 'none'}
                      checked={source?.assetId === asset.assetId}
                      onChange={() => setSource(asset)}
                    />
                    <i className="nc-src-dot" aria-hidden="true" />
                    <span
                      className="nc-chain"
                      data-chain={chain.toLowerCase()}
                      aria-hidden="true"
                    />
                    <span className="nc-src-name">{chain}</span>
                    <span className="nc-src-price">
                      {Number.isFinite(price) ? (
                        <>
                          <b>{price.toFixed(4)}</b> {token}
                        </>
                      ) : (
                        String(quote.amountInFormatted)
                      )}
                    </span>
                  </label>
                );
              })}
            </div>
          )}
          {rows && busy !== 'sources' && listed.length === 0 && (
            <p className="nc-msg" data-tone="warn" data-testid="intents-no-sources">
              1Click offers no source for this amount right now.
            </p>
          )}
          <label className="nc-field">
            <span>Refund address on the source chain</span>
            <input
              type="text"
              value={refund}
              data-testid="intents-refund"
              disabled={stage !== 'none'}
              onChange={(e) => setRefund(e.target.value)}
            />
          </label>
        </li>
        <li data-s={stepState(1)}>
          <h2>Payout account</h2>
          <p className="nc-cap nc-cap-first">
            The vault pays this NEAR account when you close the note or exit. It is written into the
            deposit message and cannot be changed. Use a fresh account.
          </p>
          <label className="nc-field">
            <span>Payout account</span>
            <input
              type="text"
              value={payout}
              data-testid="intents-payout"
              disabled={stage !== 'none'}
              onChange={(e) => setPayout(e.target.value.trim())}
            />
          </label>
          {problem && (
            <p className="nc-msg" data-tone="err" data-testid="intents-payout-problem">
              {problem}
            </p>
          )}
          {validPayout && stage === 'none' && (
            <div className="nc-pay">
              <div className="nc-fp">
                <small>Fingerprint of {payout}</small>
                <code data-testid="intents-fingerprint">{fingerprint}</code>
                <p>Compare this code with the same account in your wallet.</p>
              </div>
              <label className="nc-keep">
                <input
                  type="checkbox"
                  checked={confirmed}
                  data-testid="intents-fingerprint-confirm"
                  onChange={(e) => setConfirmed(e.target.checked)}
                />
                <span className="nc-keep-switch" aria-hidden="true" />
                <span className="nc-keep-text">
                  <strong>This is the account I want paid</strong>
                  <small>The RPC provider will see it next to your IP address.</small>
                </span>
              </label>
              {assessment === 'failed' && (
                <p className="nc-msg" data-tone="warn" data-testid="intents-assess-failed">
                  The account could not be looked up. Check the name, or try again in a moment.
                </p>
              )}
              {typeof assessment === 'object' && assessment !== null && (
                <>
                  {!assessment.registered && (
                    <p className="nc-msg" data-tone="warn" data-testid="intents-warn-unregistered">
                      This account is not registered with the {PROFILE.tokenSymbol} token. A payout
                      to it would wait until it is registered (call <code>storage_deposit</code> for
                      it).
                    </p>
                  )}
                  {assessment.registered && !assessment.fresh && (
                    <p className="nc-msg" data-tone="warn" data-testid="intents-warn-not-fresh">
                      This account already holds {PROFILE.tokenSymbol}, so it is not fresh. A payout
                      to it is easy to link to its history.
                    </p>
                  )}
                  {assessment.registered && assessment.fresh && (
                    <p className="nc-msg" data-tone="ok">
                      Registered with the token and holding none yet: a fresh account.
                    </p>
                  )}
                </>
              )}
            </div>
          )}
          <p className="nc-cap" data-testid="intents-costs">
            Closing a note through the operator costs the close fee ({units(pins.config.close_fee)}
            ). An exit or a payout claim needs a little NEAR for gas, which can come from any
            account.
          </p>
        </li>
        <li data-s={stepState(2)}>
          <h2>Protect and back up</h2>
          {!unlocked ? (
            active ? (
              <Unlock onReady={onUnlocked} />
            ) : null
          ) : (
            <BackupSteps
              prefix="intents-"
              created={created}
              createLabel={
                created
                  ? 'Created'
                  : busy === 'prepare'
                    ? 'Creating the note…'
                    : 'Create the secret'
              }
              createDisabled={created || !ready || busy !== ''}
              onCreate={onPrepare}
              pass={backupPass}
              onPass={setBackupPass}
              passDisabled={!created}
              passMin={MIN_PASSPHRASE_LENGTH}
              params={null}
              downloaded={downloaded || backedUp}
              downloadLabel={busy === 'download' ? 'Encrypting…' : `Download (${BACKUP_EXTENSION})`}
              downloadDisabled={
                passphraseLength(backupPass) < MIN_PASSPHRASE_LENGTH ||
                busy !== '' ||
                stage !== 'prepared'
              }
              onDownload={onDownload}
              tested={backedUp}
              testedNote={
                <small data-testid="intents-backup-tested">
                  Backup downloaded and test-restored. The note secret is stored on this device.
                </small>
              }
              chooseLabel={backedUp ? 'Restores' : 'Choose the file'}
              chooseDisabled={stage !== 'prepared' || !downloaded || busy !== ''}
              onFile={onChooseBackup}
              fileReady={stage === 'prepared' && downloaded}
            />
          )}
        </li>
        <li data-s={stepState(3)}>
          <h2>Quote</h2>
          <div
            className="nc-actions nc-actions-first"
            hidden={attempt !== null && !expired && !error}
          >
            <Button
              primary
              testId="intents-quote"
              disabled={
                stage !== 'backed_up' || shown || busy !== '' || (attempt !== null && !expired)
              }
              onClick={onQuote}
            >
              {busy === 'quote' ? 'Asking 1Click…' : 'Get the quote'}
            </Button>
            {expired && (
              <Button testId="intents-requote" disabled={busy !== ''} onClick={onQuote}>
                Get a new quote
              </Button>
            )}
          </div>
          {error && (
            <p className="nc-msg" data-tone="err" role="alert" data-testid="intents-error">
              {error}
            </p>
          )}
          {attempt && !expired && (
            <div data-testid="intents-review">
              <Facts
                rows={[
                  [
                    'You pay',
                    <strong data-testid="intents-pay-amount">
                      {String(q.amountInFormatted)} {attempt.source.split(' on ')[0]}
                    </strong>,
                  ],
                  ['On', attempt.source.split(' on ')[1] ?? attempt.source],
                  [
                    'You receive',
                    <>
                      {String(q.amountOutFormatted)} {PROFILE.tokenSymbol}
                    </>,
                  ],
                  [
                    'Valid until',
                    <strong data-testid="intents-deadline">{deadlineText(q.deadline)}</strong>,
                  ],
                  ['Refund to', <code data-testid="intents-refund-shown">{attempt.refund}</code>],
                ]}
              />
              <Fold title="How the cost is made up">
                <p className="nc-cap nc-cap-first" data-testid="intents-cost">
                  You pay about <strong>{String(q.amountInFormatted)}</strong> ({attempt.source}).{' '}
                  <strong>{String(q.amountOutFormatted)}</strong> {PROFILE.tokenSymbol} is delivered
                  ({units(price)}: the denomination plus the note fee). The difference is the all-in
                  cost. It includes 1Click’s documented fee for keyless use, {KEYLESS_FEE_BPS} basis
                  points of the amount delivered; the quote does not itemise it, so no split is
                  shown.
                </p>
                <p className="nc-cap" data-testid="intents-fees">
                  {fees.length === 0
                    ? 'The quote lists no other fees.'
                    : `Other fees the quote lists: ${fees.map(([n, v]) => `${n}=${JSON.stringify(v)}`).join(', ')}.`}
                </p>
                <p className="nc-cap" data-testid="intents-trust">
                  {TRUST_NOTE}
                </p>
              </Fold>
              {!shown && (
                <div className="nc-actions">
                  <Button
                    primary
                    testId="intents-show-address"
                    disabled={busy !== ''}
                    onClick={onShowAddress}
                  >
                    Show the deposit address
                  </Button>
                </div>
              )}
            </div>
          )}
        </li>
        <li data-s={stepState(4)} data-locked={!shown && !accepted && !booked}>
          <h2>Pay</h2>
          {!shown && !accepted && !booked && (
            <p className="nc-cap nc-cap-first">
              The deposit address is shown once you accept the quote.
            </p>
          )}
          {shown && attempt && (
            <div data-testid="intents-address-panel">
              <p className="nc-cap nc-cap-first">
                Pay from your own wallet on the source chain. Send exactly{' '}
                <strong>{String(q.amountInFormatted)}</strong> to:
              </p>
              <div className="nc-fp nc-deposit">
                <div className="nc-deposit-head">
                  <small>Deposit address</small>
                  <button
                    type="button"
                    className="nc-link"
                    onClick={() => {
                      void navigator.clipboard
                        ?.writeText(String(q.depositAddress))
                        .then(() => {
                          setCopied(true);
                          window.setTimeout(() => setCopied(false), 1600);
                        })
                        .catch(() => undefined);
                    }}
                  >
                    {copied ? 'Copied' : 'Copy'}
                  </button>
                </div>
                <code data-testid="intents-address">{String(q.depositAddress)}</code>
                {typeof q.depositMemo === 'string' && q.depositMemo !== '' && (
                  <small>
                    with memo <code data-testid="intents-memo">{q.depositMemo}</code>
                  </small>
                )}
              </div>
              <p className="nc-status" data-testid="intents-progress">
                <i aria-hidden="true" />
                {progress || 'Waiting for your payment.'}
              </p>
              {pendingReason && !accepted && !booked && (
                <p className="nc-msg" data-tone="warn" data-testid="intents-pending">
                  The deposit is not accepted yet: {pendingReason}.
                </p>
              )}
              <Fold title="If delivery is delayed">
                <p className="nc-cap nc-cap-first" data-testid="intents-route">
                  {ROUTE_FALLBACK}
                </p>
              </Fold>
            </div>
          )}

          {twoStep && arrived && !accepted && attempt?.via && note && (
            <div data-testid="intents-second-step">
              <p className="nc-cap">
                The {PROFILE.tokenSymbol} is in <strong>{attempt.via}</strong>. Sign the transfer to
                the vault in your wallet.
              </p>
              {refunded && (
                <p className="nc-msg" data-tone="warn" data-testid="intents-refunded">
                  The vault did not accept the transfer and the token returned the{' '}
                  {PROFILE.tokenSymbol}. It is still in {attempt.via}, and this note is unspent. You
                  can deposit again.
                </p>
              )}
              <div className="nc-actions">
                <Button
                  primary
                  testId="intents-sign"
                  disabled={busy !== '' || note.phase === 'sent' || account !== attempt.via}
                  onClick={onSign}
                >
                  {busy === 'sign'
                    ? 'Waiting for the wallet…'
                    : refunded
                      ? 'Deposit again'
                      : 'Sign the transfer'}
                </Button>
              </div>
              {account !== attempt.via && <p className="nc-cap">Connect {attempt.via} to sign.</p>}
            </div>
          )}

          {waiting !== undefined && note && (
            <p className="nc-msg" data-testid="intents-pending-payout">
              {waiting === '0'
                ? `Nothing is pending in the vault for ${note.payout}.`
                : `${units(waiting)} is pending in the vault for ${note.payout}. This is an account total across deposits and does not confirm a note.`}{' '}
              Reading it showed the payout account to the RPC provider, next to your IP address.
            </p>
          )}

          {(booked ?? accepted)?.bookings.length ? (
            <div className="nc-notice" data-testid="intents-booked">
              <strong>
                {booked ? 'The vault booked this delivery without a note' : 'The vault also booked'}
              </strong>
              {(booked ?? accepted)?.bookings.map((b) => (
                <p key={`${b.event}${b.reason}${b.amount}`} data-testid="intents-booking">
                  {explainBooking(b)}
                </p>
              ))}
              {(booked ?? accepted)?.bookings.some((b) => b.event === 'payout_pending') && note && (
                <>
                  <div className="nc-actions">
                    <Button
                      testId="intents-claim"
                      disabled={!wallet.connected || leave.busy !== null}
                      onClick={() => void leaveStore.claim(note)}
                    >
                      Claim payout
                    </Button>
                  </div>
                  <p className="nc-cap">
                    {!wallet.connected && 'Connect a wallet above to send it. '}
                    {leave.message}
                    {leave.error && <span> {leave.error}</span>}
                  </p>
                </>
              )}
            </div>
          ) : null}

          {accepted && note && (
            <div className="nc-done nc-accepted" data-testid="intents-accepted">
              <Chip tone="good">deposit accepted</Chip>
              <p>
                Note <strong data-testid="intents-note-id">#{accepted.event.noteId}</strong> expires{' '}
                <strong>{formatDate(accepted.event.expiry)}</strong>. The deposit event was read
                from the destination transaction’s receipts.
              </p>
              <ul>
                {note.txHash && (
                  <li>
                    <a
                      data-testid="intents-explorer-link"
                      href={PROFILE.explorerTxUrl(note.txHash)}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      Open the deposit receipt in the explorer
                    </a>
                    <small>
                      On this path no wallet shows the payout account, so read it there: it must be{' '}
                      <strong>{note.payout}</strong>.
                    </small>
                  </li>
                )}
                <li>
                  <Button
                    testId="intents-calendar"
                    onClick={() =>
                      download(
                        expiryCalendar(accepted.event.expiry, crypto.randomUUID(), Date.now()),
                        'note-expiry.ics',
                        'text/calendar',
                      )
                    }
                  >
                    Download a calendar reminder for the expiry
                  </Button>
                  <small>
                    The reminder has a generic title and no note data. Download a fresh backup from
                    the Notes tab: it now holds the note’s wallet state. Wait a while before your
                    first chat.
                  </small>
                </li>
              </ul>
            </div>
          )}
        </li>
      </ol>
      {created && !accepted && !booked && (
        <div className="nc-intents-cancel" data-testid="intents-cancel-row">
          {note?.phase === 'sent' ? (
            <p>The payment has reached the vault. It cannot be cancelled here.</p>
          ) : cancelling ? (
            <>
              <p>
                {shown
                  ? 'Cancel this funding? If you already sent money, do not cancel: it can still arrive. Keep your backup file and restore it from Notes.'
                  : 'Cancel this funding and start again? Nothing has been paid.'}
              </p>
              <div className="nc-actions nc-actions-first">
                <button
                  type="button"
                  className="nc-button nc-danger nc-danger-solid"
                  data-testid="intents-cancel-confirm"
                  disabled={busy !== ''}
                  onClick={cancelFunding}
                >
                  Yes, cancel
                </button>
                <Button onClick={() => setCancelling(false)}>Keep going</Button>
              </div>
            </>
          ) : (
            <button
              type="button"
              className="nc-button nc-danger"
              data-testid="intents-cancel"
              disabled={busy !== ''}
              onClick={() => (shown ? setCancelling(true) : cancelFunding())}
            >
              Cancel and start over
            </button>
          )}
        </div>
      )}
    </section>
  );
}
