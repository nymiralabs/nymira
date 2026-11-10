import { useEffect, useMemo, useRef, useState } from 'react';
import fundIllustration from '../marketing/assets/app-fund.webp';
import {
  BackupSteps,
  Button,
  Chip,
  Facts,
  FundSide,
  Screen,
  Strip,
  Waiting,
} from '../marketing/kit';
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
import { QUOTE_ONLY } from '../funding/intents';
import {
  QuoteBook,
  dryQuotesEnabled,
  intentsPathsEnabled,
  type QuoteOutcome,
} from '../funding/oneclick';
import type { NoteRecord } from '../store/types';
import { isAccountId } from '../wallet/client';
import { download, formatDate, formatUnits, readFile } from './format';
import { Intents } from './Intents';
import { rpc, useWallet } from './services';
import { WalletConnect } from './WalletConnect';
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
  if (check === null) return <Waiting title="Checking the vault against this build" />;
  if (check.status === 'ok') {
    return (
      <p className="nc-cap" data-testid="vault-ok">
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
    <div className="nc-notice" role="alert" data-testid="vault-warning" data-status={check.status}>
      <p>{text}</p>
      <div className="nc-actions">
        <Button onClick={onRecheck}>Check again</Button>
      </div>
    </div>
  );
}

function Quotes({ denomination }: { denomination: string }) {
  const [on, setOn] = useState(false);
  const [rows, setRows] = useState<QuoteOutcome[] | null>(null);
  const book = useMemo(
    () => new QuoteBook(PROFILE.oneClick.baseUrl, PROFILE.oneClick.signerKey),
    [],
  );
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
    <section className="nc-intents" data-testid="quotes">
      <p className="nc-intents-line" data-testid="quote-only">
        {QUOTE_ONLY}
      </p>
      <label className="nc-keep">
        <input
          type="checkbox"
          checked={on}
          data-testid="quotes-toggle"
          onChange={(e) => setOn(e.target.checked)}
        />
        <span className="nc-keep-switch" aria-hidden="true" />
        <span className="nc-keep-text">
          <strong>Show live prices</strong>
          <small data-testid="quotes-notice">
            Asking 1Click for a quote shows it your IP address. This page cannot use Tor, so Tor
            Browser is recommended. The request carries a dummy refund address and placeholder
            values, never your note.
          </small>
        </span>
      </label>
      {on && rows && (
        <div className="nc-src-list">
          {rows.map((row) => {
            const [token = '', chain = row.label] = row.label.split(' on ');
            const price =
              'quote' in row ? Number.parseFloat(String(row.quote.amountInFormatted)) : NaN;
            return (
              <div key={row.label} className="nc-src nc-src-static" data-testid="quote-row">
                <span className="nc-chain" data-chain={chain.toLowerCase()} aria-hidden="true" />
                <span className="nc-src-name">{chain}</span>
                <span className="nc-src-price">
                  {'quote' in row ? (
                    <>
                      <b>
                        {Number.isFinite(price)
                          ? price.toFixed(4)
                          : String(row.quote.amountInFormatted)}
                      </b>{' '}
                      {token}
                    </>
                  ) : (
                    row.error
                  )}{' '}
                  <i>quote only</i>
                </span>
              </div>
            );
          })}
          {PROFILE.oneClick.shortlist.slice(rows.length).map((asset) => {
            const [, chain = asset.label] = asset.label.split(' on ');
            return (
              <div key={asset.label} className="nc-src nc-src-static" data-testid="quote-pending">
                <span className="nc-chain" data-chain={chain.toLowerCase()} aria-hidden="true" />
                <span className="nc-src-name">{chain}</span>
                <span className="nc-src-price">
                  <i>asking 1Click…</i>
                </span>
              </div>
            );
          })}
        </div>
      )}
      {!(on && rows) && (
        <div className="nc-src-list" data-testid="quote-sources">
          {PROFILE.oneClick.shortlist.map((asset) => {
            const [, chain = asset.label] = asset.label.split(' on ');
            return (
              <div key={asset.label} className="nc-src nc-src-static">
                <span className="nc-chain" data-chain={chain.toLowerCase()} aria-hidden="true" />
                <span className="nc-src-name">{chain}</span>
                <span className="nc-src-price">
                  <i>turn on live prices to see the cost</i>
                </span>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

type Persist = 'unknown' | 'granted' | 'denied' | 'unsupported';

export function Fund({
  vault,
  recheck,
  onChat,
  onNotes,
  onFunded,
}: {
  vault: VaultCheck | null;
  recheck: () => void;
  onChat: () => void;
  onNotes: () => void;
  onFunded: () => void;
}) {
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
  const [intentsLocked, setIntentsLocked] = useState(false);
  const [acceptance, setAcceptance] = useState<Acceptance | null>(null);
  const [rpcRead, setRpcRead] = useState(false);
  const [route, setRoute] = useState<'near' | 'other'>(() => {
    try {
      return sessionStorage.getItem('nymira.route') === 'other' ? 'other' : 'near';
    } catch {
      return 'near';
    }
  });
  useEffect(() => {
    try {
      sessionStorage.setItem('nymira.route', route);
    } catch {
      // Not remembered: a reload starts on the NEAR route.
    }
  }, [route]);
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

  const onSend = () =>
    act('send', async () => {
      if (!note) return;
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
      if (result.status === 'accepted') onFunded();
    });

  const onCalendar = () => {
    if (note?.expiry === undefined) return;
    download(
      expiryCalendar(note.expiry, crypto.randomUUID(), Date.now()),
      'note-expiry.ics',
      'text/calendar',
    );
  };

  const stage = note?.phase ?? 'none';
  const created = stage !== 'none' && stage !== 'rejected';
  const backedUp = created && stage !== 'prepared';
  const accepted = acceptance?.status === 'accepted' ? acceptance : null;
  const otherRoute = intentsPathsEnabled(PROFILE) || dryQuotesEnabled(PROFILE);
  const scale = 10 ** PROFILE.tokenDecimals;
  const amount = Number(BigInt(denomination)) / scale;
  const biggest = Math.max(...(pins?.config.denominations ?? [denomination]).map(Number));

  if (!pins) {
    return (
      <Screen
        testId="fund"
        title="Fund a note"
        lead="This build does not carry the pins of a deployed vault, so there is nothing to fund yet."
        illustration={fundIllustration}
      >
        <VaultBanner check={vault} onRecheck={recheck} />
      </Screen>
    );
  }

  const sizeChoice = (locked: boolean, via: 'near' | 'other') => (
    <div className="nc-choices" role="radiogroup" aria-label="Denomination">
      {pins.config.denominations.map((d) => (
        <label key={d} className="nc-tile">
          <input
            type="radio"
            name={`denomination-${via}`}
            value={d}
            checked={denomination === d}
            disabled={locked}
            data-testid={via === 'near' ? `denomination-${d}` : `intents-denomination-${d}`}
            onChange={() => setDenomination(d)}
          />
          <strong>{units(d)}</strong>
          <Strip n={11} solid={Math.round((Number(d) / biggest) * 10)} reserved={1} size="sm" />
          <small>
            You send {units(notePrice(d))}: {units(d)} is the note, {units(fee)} is the fee.
          </small>
        </label>
      ))}
    </div>
  );

  const flags = [Boolean(sender), Boolean(sender), payoutReady, backedUp, accepted !== null];
  const now = busy === 'send' ? 4 : flags.findIndex((f) => !f);
  const stepState = (i: number) => (flags[i] ? 'done' : i === now ? 'now' : 'next');
  const ready = stage === 'backed_up' && vaultOk;

  return (
    <Screen
      testId="fund"
      title="Fund a note"
      lead={`Deposit ${PROFILE.tokenSymbol} from a NEAR wallet in a fixed size. The note secret is made in this browser. It leaves only inside an encrypted backup you download.`}
      illustration={fundIllustration}
      side={
        <FundSide
          amount={amount}
          unit={PROFILE.tokenSymbol}
          strip={{
            solid: accepted ? 20 : 0,
            reserved: accepted ? 1 : 0,
            mode: accepted ? undefined : busy === 'send' ? 'busy' : 'idle',
            ready: (flags.filter(Boolean).length / flags.length) * 20,
          }}
          yours={`${amount.toFixed(2)} yours · cells of ${(amount / 20).toFixed(3)}`}
          facts={[
            ['Denomination', <span data-testid="price-denomination">{units(denomination)}</span>],
            ['Note fee', <span data-testid="price-fee">{units(fee)}</span>],
            ['You send', <strong data-testid="price-total">{units(price)}</strong>],
          ]}
          factsTestId="price"
          caption="The fee is not refunded. Leases are charged against the rest, and you get what is left back when you close or exit. A note not closed in time goes to the treasury."
          closes={[
            ['Close or exit pays', sender === '' ? 'connect a wallet' : payout || '…'],
            ['Expires', `${pins.config.note_ttl / 86400} days after the deposit`],
          ]}
          leaves={
            <>
              <p className="nc-cap nc-cap-first" data-testid="tor-notice">
                This web app cannot use Tor. The RPC provider sees your IP address and the time of
                each read. For IP protection use Tor Browser, or run <code>nymirad</code> on your
                own machine.
              </p>
              <p className="nc-cap" data-testid="receipt-note">
                After the transfer the app reads the <code>deposit</code> event from your
                transaction’s receipts. If your wallet returns the full receipt tree, the app uses
                that. Otherwise it asks the RPC provider for the transaction, and the provider then
                sees your IP address, your account, the transaction hash (so the note ID, the
                commitment and the payout account) and the time. The app never calls{' '}
                <code>note_status</code>.
              </p>
            </>
          }
          limits={[
            ...(vault?.status === 'ok'
              ? ([['Open deposits', `capped at ${units(vault.limits.tvlCap)}`]] as const)
              : []),
            ['Sizes', pins.config.denominations.map(units).join(' or ')],
          ]}
          testnetNote={
            PROFILE.network === 'testnet' ? (
              <p className="nc-cap" data-testid="testnet-notice">
                This is a testnet deployment with test funds only. The anonymity set is small and
                made of test notes, and the contract has had no independent review.
              </p>
            ) : undefined
          }
        />
      }
    >
      <div className="nc-fund-top">
        <p className="nc-terms" data-testid="terms-fund">
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
        {otherRoute && (
          <fieldset className="nc-route">
            <legend className="nc-sr">Where is the money coming from?</legend>
            <div className="nc-seg">
              <label>
                <input
                  type="radio"
                  name="route"
                  data-testid="fund-source-near"
                  checked={route === 'near'}
                  onChange={() => setRoute('near')}
                />
                <span>
                  {PROFILE.tokenSymbol} on
                  <span className="nc-logo nc-logo-near" role="img" aria-label="NEAR" />
                </span>
              </label>
              <label>
                <input
                  type="radio"
                  name="route"
                  data-testid="fund-source-other"
                  checked={route === 'other'}
                  onChange={() => setRoute('other')}
                />
                <span>
                  <span className="nc-logo nc-logo-eth" aria-hidden="true" />
                  Another chain
                  {!intentsPathsEnabled(PROFILE) && <em className="nc-seg-tag">quote only</em>}
                </span>
              </label>
            </div>
          </fieldset>
        )}
      </div>
      <div hidden={route !== 'near'}>
        <ol className="nc-steps">
          <li data-s={stepState(0)}>
            <h2>Wallet</h2>
            <WalletConnect busy={busy !== ''} />
            {route === 'near' && <VaultBanner check={vault} onRecheck={recheck} />}
          </li>
          <li data-s={stepState(1)}>
            <h2>Size</h2>
            {sizeChoice(created, 'near')}
            {PROFILE.network === 'testnet' && (
              <p className="nc-cap" data-testid="faucet">
                Test {PROFILE.tokenSymbol} is Circle's testnet token. Get it at{' '}
                <a href={PROFILE.faucetUrl ?? '#'} target="_blank" rel="noopener noreferrer">
                  faucet.circle.com
                </a>{' '}
                (choose Near Testnet; 20 {PROFILE.tokenSymbol} per request, one request per address
                every 2 hours). The token only accepts accounts that paid for storage: call{' '}
                <code>storage_deposit</code> on it once for your account before the transfer (about
                0.00125 NEAR attached; the command is in JUDGES.md), and for the payout account if
                it is another one.
              </p>
            )}
          </li>
          <li data-s={stepState(2)}>
            <h2>Payout account</h2>
            <p className="nc-cap nc-cap-first">
              When you close the note, or leave through an exit, the vault pays this account. It is
              fixed at deposit.
            </p>
            <div className="nc-radios" role="radiogroup" aria-label="Payout account">
              <label className="nc-tile">
                <input
                  type="radio"
                  name="payout"
                  checked={payoutMode === 'self'}
                  data-testid="payout-self"
                  onChange={() => setPayoutMode('self')}
                />
                <span>My sending account</span>
                <code data-testid="payout-account">{sender || 'connect a wallet'}</code>
              </label>
              <label className="nc-tile">
                <input
                  type="radio"
                  name="payout"
                  checked={payoutMode === 'other'}
                  data-testid="payout-other"
                  onChange={() => setPayoutMode('other')}
                />
                <span>Another account</span>
                <code>{other.trim() || 'a fresh account'}</code>
              </label>
            </div>
            {payoutMode === 'self' ? (
              <p className="nc-cap" data-testid="payout-default-note">
                The deposit message will not name a payout account, so there is nothing in it to
                alter. For a payout that is not linked to this account on chain, use a fresh account
                instead.
              </p>
            ) : (
              <div className="nc-other">
                <label className="nc-field">
                  <span>Payout account</span>
                  <input
                    type="text"
                    value={other}
                    placeholder="fresh-account.testnet"
                    data-testid="payout-input"
                    onChange={(e) => setOther(e.target.value)}
                  />
                </label>
                {other !== '' && !isAccountId(payout) && (
                  <p className="nc-msg" data-tone="err">
                    Not a valid NEAR account id.
                  </p>
                )}
                {isAccountId(payout) && payout === sender && (
                  <p className="nc-msg">That is your sending account: choose the first option.</p>
                )}
                {isAccountId(payout) && payout !== sender && (
                  <div className="nc-pay">
                    <div className="nc-fp" data-testid="fingerprint-box">
                      <small>Fingerprint of {payout}</small>
                      <code data-testid="fingerprint">{fingerprint}</code>
                      <p>Compare this code with the same account in your wallet.</p>
                    </div>
                    <label className="nc-keep">
                      <input
                        type="checkbox"
                        checked={fingerprintOk}
                        data-testid="fingerprint-confirm"
                        onChange={(e) => setFingerprintOk(e.target.checked)}
                      />
                      <span className="nc-keep-switch" aria-hidden="true" />
                      <span className="nc-keep-text">
                        <strong>This is the account I want paid</strong>
                        <small>The RPC provider will see it next to your IP address.</small>
                      </span>
                    </label>
                    {assessment === 'checking' && <p className="nc-msg">Checking the account…</p>}
                    {assessment === 'failed' && (
                      <p className="nc-msg" data-tone="warn" data-testid="assess-failed">
                        The account could not be looked up. Check the name, or try again in a
                        moment.
                      </p>
                    )}
                    {typeof assessment === 'object' && assessment !== null && (
                      <>
                        {!assessment.registered && (
                          <p className="nc-msg" data-tone="warn" data-testid="warn-unregistered">
                            This account is not registered with the {PROFILE.tokenSymbol} token. A
                            payout to it would wait until it is registered (call{' '}
                            <code>storage_deposit</code> for it).
                          </p>
                        )}
                        {assessment.registered && !assessment.fresh && (
                          <p className="nc-msg" data-tone="warn" data-testid="warn-not-fresh">
                            This account already holds {PROFILE.tokenSymbol}, so it is not fresh. A
                            payout to it is easy to link to its history.
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
              </div>
            )}
          </li>
          <li data-s={stepState(3)}>
            <h2>Protect and back up</h2>
            {!unlocked ? (
              route === 'near' ? (
                <Unlock onReady={() => setUnlocked(true)} />
              ) : null
            ) : (
              <>
                <p className="nc-cap nc-cap-first">
                  Nothing is sent until you have a backup that opens.
                </p>
                <BackupSteps
                  created={created}
                  createLabel={
                    created
                      ? 'Created'
                      : busy === 'prepare'
                        ? 'Creating the note…'
                        : 'Create the secret'
                  }
                  createDisabled={
                    created || !vaultOk || !wallet.connected || !payoutReady || busy !== ''
                  }
                  onCreate={onPrepare}
                  createNote={
                    created && (
                      <small data-testid="persist-status">
                        Persistent storage:{' '}
                        {persist === 'granted'
                          ? 'granted. The browser will not clear this site’s data under disk pressure.'
                          : persist === 'denied'
                            ? 'not granted. The browser may clear this site’s data, so keep the backup file safe.'
                            : 'not supported by this browser. Keep the backup file safe.'}
                      </small>
                    )
                  }
                  pass={backupPass}
                  onPass={setBackupPass}
                  passDisabled={!created}
                  passMin={MIN_PASSPHRASE_LENGTH}
                  params={
                    <small data-testid="backup-params">
                      Encrypted with {ARGON2_SENTENCE}. Do not keep the file in a synced folder:
                      whoever opens it can spend the note.
                    </small>
                  }
                  downloaded={downloaded || backedUp}
                  downloadLabel={
                    busy === 'download' ? 'Encrypting…' : `Download (${BACKUP_EXTENSION})`
                  }
                  downloadDisabled={
                    passphraseLength(backupPass) < MIN_PASSPHRASE_LENGTH ||
                    busy !== '' ||
                    stage !== 'prepared'
                  }
                  onDownload={onDownload}
                  tested={backedUp}
                  testedNote={
                    <small data-testid="backup-tested">Backup downloaded and test-restored.</small>
                  }
                  chooseLabel={backedUp ? 'Restores' : 'Choose the file'}
                  chooseDisabled={stage !== 'prepared' || !downloaded || busy !== ''}
                  onFile={onChooseBackup}
                  fileReady={stage === 'prepared' && downloaded}
                />
              </>
            )}
          </li>
          <li data-s={stepState(4)} data-locked={!created || stage === 'prepared'}>
            <h2>Send</h2>
            {accepted && note ? (
              <div
                className="nc-result nc-accepted nc-done"
                data-tone="good"
                data-testid="accepted"
              >
                <Chip tone="good">deposit accepted</Chip>
                <p className="nc-done-line">
                  <span>
                    Note <strong data-testid="note-id">#{accepted.event.noteId}</strong>
                  </span>
                  <span>
                    expires{' '}
                    <strong data-testid="note-expiry">{formatDate(accepted.event.expiry)}</strong>
                  </span>
                  <span>
                    {accepted.source === 'wallet'
                      ? 'receipt from your wallet'
                      : accepted.source === 'rpc'
                        ? 'receipt from the RPC provider'
                        : 'found on the vault itself, your wallet never answered'}
                  </span>
                </p>
                {accepted.source === 'chain' && (
                  <p className="nc-msg" data-tone="warn" data-testid="chain-found-warning">
                    Without the wallet’s receipt the app could not check the payout account. Open
                    the transaction in your wallet’s history or the explorer and check that it pays{' '}
                    <strong>{note.payout}</strong>.
                  </p>
                )}
                <ul className="nc-done-list">
                  {note.txHash && (
                    <li>
                      <a
                        data-testid="explorer-link"
                        href={PROFILE.explorerTxUrl(note.txHash)}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        Open the deposit receipt in the explorer
                      </a>
                      <small>
                        Check that the payout account is <strong>{note.payout}</strong>. This page
                        is the code you would have to trust to tell you that, so look at the chain
                        itself.
                      </small>
                    </li>
                  )}
                  <li>
                    <Button testId="calendar" onClick={onCalendar}>
                      Add the expiry to a calendar
                    </Button>
                    <small>
                      Generic title, no note data. A calendar that syncs to a provider tells that
                      provider the date.
                    </small>
                  </li>
                  <li data-testid="wait-hint">
                    <span>Wait a while before your first chat.</span>
                    <small>
                      A lease opened right after funding is easy to link to the deposit by timing.
                    </small>
                  </li>
                </ul>
                <div className="nc-actions">
                  <Button primary onClick={onChat}>
                    Chat with this note
                  </Button>
                  <Button onClick={onNotes}>Download a fresh backup in Notes</Button>
                  <small>It now holds the note’s wallet state.</small>
                </div>
              </div>
            ) : (
              <>
                <div className="nc-actions nc-actions-first">
                  <Button primary testId="send" disabled={!ready || busy !== ''} onClick={onSend}>
                    {busy === 'send' ? 'Sending…' : `Send ${units(price)}`}
                  </Button>
                </div>
                {busy === 'send' && (
                  <p className="nc-status" data-testid="send-waiting">
                    <i aria-hidden="true" />
                    Waiting for your wallet. Approve the transaction in its window; the app then
                    reads the receipt.
                  </p>
                )}
                {!ready && (
                  <p className="nc-cap">
                    {!sender
                      ? 'Connect a wallet first.'
                      : !payoutReady
                        ? 'Choose where the payout goes.'
                        : !vaultOk
                          ? 'Funding is stopped until the vault check passes.'
                          : 'Create the secret, download the backup and test it first.'}
                  </p>
                )}
              </>
            )}
            {error && (
              <div className="nc-result" data-tone="err" role="alert">
                <div className="nc-result-head">
                  <i aria-hidden="true" />
                  <strong>Could not complete this step</strong>
                </div>
                <p data-testid="fund-error">{error}</p>
                {/wallet|transfer|deposit|transaction/i.test(error) && (
                  <div className="nc-actions nc-actions-first">
                    <Button onClick={onNotes}>Look in Notes</Button>
                  </div>
                )}
              </div>
            )}
            {rpcRead && (
              <p className="nc-cap" data-testid="rpc-read-notice">
                The receipt was read from the RPC provider: it saw your IP address, account, the
                transaction hash and the time.
              </p>
            )}
            {acceptance?.status === 'rejected' && (
              <div className="nc-result" data-tone="err">
                <div className="nc-result-head">
                  <i aria-hidden="true" />
                  <strong>Not accepted</strong>
                </div>
                <p data-testid="rejected">
                  The deposit was not accepted: {acceptance.reason}. The token should return the
                  USDC; check your wallet.
                </p>
              </div>
            )}
            {acceptance?.status === 'pending' && (
              <div className="nc-result" data-tone="warn">
                <div className="nc-result-head">
                  <i aria-hidden="true" />
                  <strong>Not confirmed yet</strong>
                </div>
                <p data-testid="pending">
                  The deposit is not confirmed yet: {acceptance.reason}. You can look it up from the
                  Notes tab.
                </p>
                <div className="nc-actions nc-actions-first">
                  <Button onClick={onNotes}>Look it up in Notes</Button>
                </div>
              </div>
            )}
          </li>
        </ol>
      </div>
      {otherRoute && (
        <div hidden={route !== 'other'}>
          {route === 'other' && <VaultBanner check={vault} onRecheck={recheck} />}
          {intentsPathsEnabled(PROFILE) && (
            <Intents
              vault={vault}
              denomination={denomination}
              size={sizeChoice(intentsLocked, 'other')}
              onLocked={setIntentsLocked}
              unlocked={unlocked}
              active={route === 'other'}
              onUnlocked={() => setUnlocked(true)}
            />
          )}
          {!intentsPathsEnabled(PROFILE) && (
            <>
              <p className="nc-cap nc-picture-note" data-testid="intents-preview-note">
                The steps of the cross-chain payment, as they look where it runs. They are a picture
                here: nothing in them works on {PROFILE.network}.
              </p>
              <div className="nc-picture" inert>
                <Intents
                  vault={vault}
                  denomination={denomination}
                  size={sizeChoice(true, 'other')}
                  onLocked={() => undefined}
                  unlocked={unlocked}
                  active={false}
                  onUnlocked={() => undefined}
                  preview
                />
              </div>
            </>
          )}
          {dryQuotesEnabled(PROFILE) && <Quotes denomination={denomination} />}
        </div>
      )}
    </Screen>
  );
}
