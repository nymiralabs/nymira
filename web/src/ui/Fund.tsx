import { useEffect, useMemo, useRef, useState } from 'react';
import { PROFILE } from '../profile';
import { assessPayout, accountFingerprint, type PayoutAssessment } from '../chain/token';
import type { VaultCheck } from '../chain/vault';
import { expiryCalendar } from '../backup/ics';
import { backupFileName, BACKUP_EXTENSION } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked } from '../crypto/wrap';
import {
  confirmDeposit,
  makeBackupFile,
  notePrice,
  prepareNote,
  sendDepositWatched,
  acceptFound,
  testRestore,
  type Acceptance,
} from '../funding/flow';
import { QuoteBook, dryQuotesEnabled, type QuoteOutcome } from '../funding/oneclick';
import type { NoteRecord } from '../store/types';
import { isAccountId } from '../wallet/client';
import { download, formatDate, formatUnits, readFile } from './format';
import { Notices } from './Landing';
import { rpc, useWallet } from './services';
import { SessionLost } from './WalletConnect';
import { ARGON2_SENTENCE, Unlock } from './Unlock';

const units = (v: bigint | string) =>
  `${formatUnits(v, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`;

export function VaultBanner({
  check,
  onRecheck,
}: {
  check: VaultCheck | null;
  onRecheck: () => void;
}) {
  if (check === null) return <p className="hint">Checking the vault against this build…</p>;
  if (check.status === 'ok') {
    return (
      <p className="ok" data-testid="vault-ok">
        The vault matches what this build expects (code hash, verifying keys, settings and access
        keys).
      </p>
    );
  }
  const text =
    check.status === 'unpinned'
      ? 'This build does not carry the pins of a deployed vault, so funding is switched off.'
      : check.status === 'mismatch'
        ? `The vault differs from what this build expects: ${check.diffs.join(', ')}. Funding and chat are stopped. Do not send anything.`
        : `The vault could not be checked (${check.detail}). Funding is stopped until it can.`;
  return (
    <div className="warning" role="alert" data-testid="vault-warning" data-status={check.status}>
      <p>{text}</p>
      <button type="button" onClick={onRecheck}>
        Check again
      </button>
    </div>
  );
}

function Quotes({ denomination }: { denomination: string }) {
  const [on, setOn] = useState(false);
  const [rows, setRows] = useState<QuoteOutcome[] | null>(null);
  const book = useMemo(() => new QuoteBook(PROFILE.oneClick.baseUrl), []);
  useEffect(() => {
    if (!on) {
      setRows(null);
      return;
    }
    let live = true;
    const amount = notePrice(denomination);
    setRows([]);
    (async () => {
      for (const asset of PROFILE.oneClick.shortlist) {
        const outcome = await book.quote(asset, amount);
        if (!live) return;
        setRows((previous) => [...(previous ?? []), outcome]);
      }
    })();
    return () => {
      live = false;
    };
  }, [on, denomination, book]);

  return (
    <section data-testid="quotes">
      <h3>Funding from another chain</h3>
      <p>
        These are price quotes only. Paying from another chain is not available on testnet and was
        not tested here: the funding that runs for real is the direct {PROFILE.tokenSymbol} deposit
        above.
      </p>
      <label>
        <input
          type="checkbox"
          checked={on}
          data-testid="quotes-toggle"
          onChange={(e) => setOn(e.target.checked)}
        />
        Show live quotes
      </label>
      <p className="hint" data-testid="quotes-notice">
        Asking 1Click for a quote shows it your IP address. This page cannot use Tor, so Tor Browser
        is recommended. The request carries a dummy refund address and no note data.
      </p>
      {on && rows && (
        <ul>
          {rows.map((row) => (
            <li key={row.label} data-testid="quote-row">
              {'quote' in row ? (
                <>
                  {row.label}: about {row.quote.amountInFormatted} in, to receive{' '}
                  {units(notePrice(denomination))} <strong>quote only</strong>
                </>
              ) : (
                <>
                  {row.label}: {row.error} <strong>quote only</strong>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

type Persist = 'unknown' | 'granted' | 'denied' | 'unsupported';

export function Fund({ vault, recheck }: { vault: VaultCheck | null; recheck: () => void }) {
  const wallet = useWallet();
  const pins = PROFILE.pins;
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const [denomination, setDenomination] = useState(pins?.config.denominations[0] ?? '1000000');
  const [payoutMode, setPayoutMode] = useState<'self' | 'other'>('self');
  const [other, setOther] = useState('');
  const [fingerprint, setFingerprint] = useState('');
  const [fingerprintOk, setFingerprintOk] = useState(false);
  const [assessment, setAssessment] = useState<PayoutAssessment | 'checking' | 'failed' | null>(
    null,
  );
  const [note, setNote] = useState<NoteRecord | null>(null);
  const [backupPass, setBackupPass] = useState('');
  const [downloaded, setDownloaded] = useState(false);
  const [persist, setPersist] = useState<Persist>('unknown');
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [acceptance, setAcceptance] = useState<Acceptance | null>(null);
  const [rpcRead, setRpcRead] = useState(false);
  const [connectError, setConnectError] = useState('');
  const outcomeRef = useRef<unknown>(null);

  const sender = wallet.connected?.accountId ?? '';
  const payout = payoutMode === 'self' ? sender : other.trim();
  const vaultOk = vault?.status === 'ok';

  // Start the wallet frame as soon as the screen opens, so its wallet list is ready.
  useEffect(() => {
    void wallet.start();
  }, [wallet]);

  useEffect(() => {
    setFingerprintOk(false);
    if (payoutMode === 'other' && isAccountId(payout))
      accountFingerprint(payout).then(setFingerprint);
    else setFingerprint('');
  }, [payout, payoutMode]);

  // The payout checks name the payout account to the RPC provider; the screen says so.
  useEffect(() => {
    if (payoutMode !== 'other' || !isAccountId(payout) || !sender) {
      setAssessment(null);
      return;
    }
    let live = true;
    setAssessment('checking');
    assessPayout(rpc, PROFILE.tokenAccount, payout, sender).then(
      (a) => live && setAssessment(a),
      () => live && setAssessment('failed'),
    );
    return () => {
      live = false;
    };
  }, [payout, payoutMode, sender]);

  const price = pins ? notePrice(denomination) : 0n;
  const fee = pins ? BigInt(pins.config.note_fee) : 0n;

  const payoutReady =
    sender !== '' &&
    (payoutMode === 'self' ||
      (isAccountId(payout) &&
        payout !== sender &&
        fingerprintOk &&
        typeof assessment === 'object' &&
        assessment !== null));

  const act = async (label: string, action: () => Promise<void>) => {
    setBusy(label);
    setError('');
    try {
      await action();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy('');
    }
  };

  const onPrepare = () =>
    act('prepare', async () => {
      const record = await prepareNote({ denomination, payout, sender });
      setNote(record);
      setDownloaded(false);
      setAcceptance(null);
      if (navigator.storage?.persist) {
        setPersist((await navigator.storage.persist()) ? 'granted' : 'denied');
      } else {
        setPersist('unsupported');
      }
    });

  const onDownload = () =>
    act('download', async () => {
      const bytes = await makeBackupFile(backupPass);
      download(bytes as BlobPart, backupFileName(), 'application/octet-stream');
      setDownloaded(true);
    });

  const onChooseBackup = (file: File | undefined) => {
    if (!file || !note) return;
    act('test', async () => {
      setNote(await testRestore(note, await readFile(file), backupPass));
    });
  };

  const onSend = () => {
    // Before any await: the click is fresh, and a wallet that opens a window needs it (popup.ts).
    wallet.arm();
    return act('send', async () => {
      if (!note) return;
      try {
        const sent = await sendDepositWatched(rpc, note, wallet, PROFILE.watch);
        setNote(sent.record);
        outcomeRef.current = sent.outcome;
        const result = sent.found
          ? await acceptFound(sent.record, sent.found)
          : await confirmDeposit(rpc, sent.record, sent.outcome, {
              onRpcRead: () => setRpcRead(true),
            });
        setAcceptance(result);
        if ('record' in result) setNote(result.record);
      } finally {
        wallet.release();
      }
    });
  };

  const onCalendar = () => {
    if (note?.expiry === undefined) return;
    download(
      expiryCalendar(note.expiry, crypto.randomUUID(), Date.now()),
      'note-expiry.ics',
      'text/calendar',
    );
  };

  const stage = note?.phase ?? 'none';

  if (!pins) {
    return (
      <section data-testid="fund">
        <h2>Fund a note</h2>
        <Notices />
        <VaultBanner check={vault} onRecheck={recheck} />
      </section>
    );
  }

  return (
    <section data-testid="fund">
      <h2>Fund a note</h2>
      <p className="hint" data-testid="terms-fund">
        By funding a note you accept the{' '}
        <a
          href="https://github.com/nymiralabs/nymira/blob/main/TERMS.md"
          target="_blank"
          rel="noopener noreferrer"
        >
          terms of use
        </a>
        .
      </p>
      <Notices />
      <VaultBanner check={vault} onRecheck={recheck} />

      <h3>1. Wallet</h3>
      {wallet.connected ? (
        <p data-testid="connected">
          Connected as <strong data-testid="account">{wallet.connected.accountId}</strong>
          {wallet.connected.signature === 'valid'
            ? ' (sign-in signature checked)'
            : ' (this browser cannot check the sign-in signature)'}
        </p>
      ) : (
        <div>
          <SessionLost />
          <p>
            On {PROFILE.network}, Meteor and Intear are the supported wallets. Other wallets may
            work and are not promised.
          </p>
          <ul className="wallets">
            {wallet.wallets.map((w) => (
              <li key={w.id}>
                <button
                  type="button"
                  data-testid={`connect-${w.id}`}
                  disabled={busy !== ''}
                  onClick={() =>
                    act('connect', async () => {
                      setConnectError('');
                      try {
                        await wallet.connect(w.id);
                      } catch (e) {
                        setConnectError(e instanceof Error ? e.message : String(e));
                      }
                    })
                  }
                >
                  {w.name}
                  {w.injected ? ' (browser extension)' : ''}
                </button>
              </li>
            ))}
          </ul>
          {wallet.wallets.length === 0 && <p className="hint">Starting the wallet connection…</p>}
          {wallet.skipped.map((s) => (
            <p key={s.id} className="warning" data-testid="wallet-skipped">
              {s.id} is not offered: {s.reason}
            </p>
          ))}
          {connectError && (
            <p className="error" data-testid="connect-error">
              {connectError}
            </p>
          )}
        </div>
      )}

      <h3>2. Choose a note</h3>
      <fieldset disabled={stage !== 'none' && stage !== 'rejected'}>
        <legend>Denomination</legend>
        {pins.config.denominations.map((d) => (
          <label key={d}>
            <input
              type="radio"
              name="denomination"
              value={d}
              checked={denomination === d}
              data-testid={`denomination-${d}`}
              onChange={() => setDenomination(d)}
            />
            {units(d)}
          </label>
        ))}
      </fieldset>
      <table className="price" data-testid="price">
        <tbody>
          <tr>
            <th>Denomination (yours to spend or get back)</th>
            <td data-testid="price-denomination">{units(denomination)}</td>
          </tr>
          <tr>
            <th>Note fee (non-refundable)</th>
            <td data-testid="price-fee">{units(fee)}</td>
          </tr>
          <tr>
            <th>You send</th>
            <td data-testid="price-total">{units(price)}</td>
          </tr>
        </tbody>
      </table>
      {PROFILE.network === 'testnet' && (
        <p className="hint" data-testid="faucet">
          Test {PROFILE.tokenSymbol} is Circle's testnet token. Get it at{' '}
          <a href={PROFILE.faucetUrl ?? '#'} target="_blank" rel="noopener noreferrer">
            faucet.circle.com
          </a>{' '}
          (choose Near Testnet; 20 {PROFILE.tokenSymbol} per request, one request per address every
          2 hours). Call <code>storage_deposit</code> on the token for your account before the
          transfer, and for the payout account if it is another one.
        </p>
      )}

      <h3>3. Payout account</h3>
      <p>
        When you close the note, or leave through an exit, the vault pays this account. It is fixed
        at deposit.
      </p>
      <label>
        <input
          type="radio"
          name="payout"
          checked={payoutMode === 'self'}
          data-testid="payout-self"
          onChange={() => setPayoutMode('self')}
        />
        My sending account:{' '}
        <strong data-testid="payout-account">{sender || 'connect a wallet'}</strong>
      </label>
      <label>
        <input
          type="radio"
          name="payout"
          checked={payoutMode === 'other'}
          data-testid="payout-other"
          onChange={() => setPayoutMode('other')}
        />
        Another account
      </label>
      {payoutMode === 'self' ? (
        <p className="hint" data-testid="payout-default-note">
          The deposit message will not name a payout account, so there is nothing in it to alter.
          For a payout that is not linked to this account on chain, use a fresh account instead.
        </p>
      ) : (
        <div>
          <input
            type="text"
            aria-label="Payout account"
            value={other}
            placeholder="fresh-account.testnet"
            data-testid="payout-input"
            onChange={(e) => setOther(e.target.value)}
          />
          {other !== '' && !isAccountId(payout) && (
            <p className="error">Not a valid NEAR account id.</p>
          )}
          {isAccountId(payout) && payout === sender && (
            <p className="hint">That is your sending account: choose the first option.</p>
          )}
          {isAccountId(payout) && payout !== sender && (
            <>
              <p data-testid="fingerprint-box">
                Fingerprint of <strong>{payout}</strong>:{' '}
                <code data-testid="fingerprint">{fingerprint}</code>. Check the account letter by
                letter. A swapped account is the way this page could cost you the payout.
              </p>
              <label>
                <input
                  type="checkbox"
                  checked={fingerprintOk}
                  data-testid="fingerprint-confirm"
                  onChange={(e) => setFingerprintOk(e.target.checked)}
                />
                This is the account I want paid.
              </label>
              <p className="hint">
                Checking this account asks the RPC provider about it, so the provider sees it next
                to your IP address.
              </p>
              {assessment === 'checking' && <p className="hint">Checking the account…</p>}
              {assessment === 'failed' && (
                <p className="warning" data-testid="assess-failed">
                  The account could not be checked.
                </p>
              )}
              {typeof assessment === 'object' && assessment !== null && (
                <>
                  {!assessment.registered && (
                    <p className="warning" data-testid="warn-unregistered">
                      This account is not registered with the {PROFILE.tokenSymbol} token. A payout
                      to it would wait until it is registered (call <code>storage_deposit</code> for
                      it).
                    </p>
                  )}
                  {assessment.registered && !assessment.fresh && (
                    <p className="warning" data-testid="warn-not-fresh">
                      This account already holds {PROFILE.tokenSymbol}, so it is not fresh. A payout
                      to it is easy to link to its history.
                    </p>
                  )}
                </>
              )}
            </>
          )}
        </div>
      )}

      <h3>4. Protect and back up</h3>
      {!unlocked ? (
        <Unlock onReady={() => setUnlocked(true)} />
      ) : stage === 'none' || stage === 'rejected' ? (
        <>
          <p className="hint">
            The secret is stored encrypted before anything is sent. Then you download a backup and
            check it by opening it again: only after that does the app offer the transfer.
          </p>
          <button
            type="button"
            className="primary"
            data-testid="prepare"
            disabled={!vaultOk || !wallet.connected || !payoutReady || busy !== ''}
            onClick={onPrepare}
          >
            {busy === 'prepare' ? 'Creating the note…' : 'Create the note secret'}
          </button>
        </>
      ) : (
        <div data-testid="backup-panel">
          <p data-testid="persist-status">
            Persistent storage:{' '}
            {persist === 'granted'
              ? 'granted. The browser will not clear this site’s data under disk pressure.'
              : persist === 'denied'
                ? 'not granted. The browser may clear this site’s data, so keep the backup file safe.'
                : 'not supported by this browser. Keep the backup file safe.'}
          </p>
          <label>
            Backup passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)
            <input
              type="password"
              autoComplete="new-password"
              value={backupPass}
              data-testid="backup-passphrase"
              onChange={(e) => setBackupPass(e.target.value)}
            />
          </label>
          <p className="hint" data-testid="backup-params">
            The file is encrypted with {ARGON2_SENTENCE}, the same settings that wrap your notes. Do
            not keep it in a synced folder: a copy in the cloud can be opened by guessing the
            passphrase, and whoever opens it can spend the note.
          </p>
          <button
            type="button"
            data-testid="backup-download"
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
                data-testid="backup-file"
                disabled={busy !== ''}
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  // Cleared so choosing the same file again (after a typo) fires again.
                  e.target.value = '';
                  onChooseBackup(file);
                }}
              />
            </label>
          )}
          {stage !== 'prepared' && (
            <p className="ok" data-testid="backup-tested">
              Backup downloaded and test-restored.
            </p>
          )}
        </div>
      )}

      <h3>5. Send</h3>
      <p className="hint" data-testid="receipt-note">
        After the transfer the app reads the <code>deposit</code> event from your transaction’s
        receipts. If your wallet returns the full receipt tree, the app uses that. Otherwise it asks
        the RPC provider for the transaction, and the provider then sees your IP address, your
        account, the transaction hash (so the note ID, the commitment and the payout account) and
        the time. The app never calls <code>note_status</code>.
      </p>
      <button
        type="button"
        className="primary"
        data-testid="send"
        disabled={stage !== 'backed_up' || !vaultOk || busy !== ''}
        onClick={onSend}
      >
        {busy === 'send' ? 'Waiting for the wallet and the receipt…' : `Send ${units(price)}`}
      </button>
      {error && (
        <p className="error" role="alert" data-testid="fund-error">
          {error}
        </p>
      )}
      {rpcRead && (
        <p className="hint" data-testid="rpc-read-notice">
          The receipt was read from the RPC provider: it saw your IP address, account, the
          transaction hash and the time.
        </p>
      )}
      {acceptance?.status === 'accepted' && note && (
        <div className="ok" data-testid="accepted">
          <h3>Deposit accepted</h3>
          <p>
            Note <strong data-testid="note-id">#{acceptance.event.noteId}</strong>, expires{' '}
            <strong data-testid="note-expiry">{formatDate(acceptance.event.expiry)}</strong>.{' '}
            {acceptance.source === 'wallet'
              ? 'The receipt was read from your wallet.'
              : acceptance.source === 'rpc'
                ? 'The receipt was read from the RPC provider.'
                : 'Your wallet never answered, so the app found the note on the vault itself.'}
          </p>
          {acceptance.source === 'chain' && (
            <p className="warning" data-testid="chain-found-warning">
              Without the wallet’s receipt the app could not check the payout account. Open the
              transaction in your wallet’s history or the explorer and check that it pays{' '}
              <strong>{note.payout}</strong>.
            </p>
          )}
          {note.txHash && (
            <p>
              <a
                data-testid="explorer-link"
                href={PROFILE.explorerTxUrl(note.txHash)}
                target="_blank"
                rel="noopener noreferrer"
              >
                Open the deposit receipt in the explorer
              </a>
              . Check there that the payout account is <strong>{note.payout}</strong>. This page is
              the code you would have to trust to tell you that, so look at the chain itself.
            </p>
          )}
          <button type="button" data-testid="calendar" onClick={onCalendar}>
            Download a calendar reminder for the expiry
          </button>
          <p className="hint">
            The reminder has a generic title and no note data. A calendar that syncs to a provider
            tells that provider the date.
          </p>
          <p data-testid="wait-hint">
            Wait a while before your first chat. A lease opened right after funding is easy to link
            to the deposit by timing.
          </p>
          <p className="hint">
            Download a fresh backup from the Notes tab: it now holds the note’s wallet state.
          </p>
        </div>
      )}
      {acceptance?.status === 'rejected' && (
        <p className="error" data-testid="rejected">
          The deposit was not accepted: {acceptance.reason}. Check your wallet; a refused transfer
          is refunded by the token.
        </p>
      )}
      {acceptance?.status === 'pending' && (
        <p className="warning" data-testid="pending">
          The deposit is not confirmed yet: {acceptance.reason}. You can look it up from the Notes
          tab.
        </p>
      )}

      {dryQuotesEnabled(PROFILE) && <Quotes denomination={denomination} />}
    </section>
  );
}
