import { useCallback, useEffect, useState } from 'react';
import './notes.css';
import notesIllustration from '../marketing/assets/app-notes.webp';
import { Button, Card, Chip, Facts, Screen, Strip, clamp, statusText } from '../marketing/kit';
import type { RowNote } from '../marketing/kit';
import { LookupRow, NoteLedger, NoteLedgerRow, PassField } from '../marketing/kit-notes';
import { Count } from '../marketing/Board';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked, requireKey } from '../crypto/wrap';
import {
  acceptFound,
  attachHash,
  confirmDeposit,
  loadNotes,
  makeBackupFile,
} from '../funding/flow';
import { findDeposit } from '../funding/watch';
import { INTENTS_SIGNER, confirmIntents, explainBooking } from '../funding/intents';
import { deleteNote, listNotes, putNote } from '../store/db';
import { mergeRestored } from '../store/merge';
import type { NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { seedSampleNotes } from './dev-seed';
import { download, formatDate, formatUnits, readFile } from './format';
import { archivalRpc, rpc } from './services';
import { ARGON2_SENTENCE, Unlock } from './Unlock';

const PHASE_TEXT: Record<NoteRecord['phase'], string> = {
  prepared: 'created, not sent',
  backed_up: 'backed up, not sent',
  sending: 'sending (interrupted?)',
  sent: 'sent, not confirmed',
  accepted: 'accepted',
  rejected: 'not accepted',
};

const TONE: Record<NoteRecord['phase'], 'plain' | 'good' | 'warn'> = {
  prepared: 'plain',
  backed_up: 'plain',
  sending: 'warn',
  sent: 'plain',
  accepted: 'good',
  rejected: 'warn',
};

/** Share of the note's life that has passed, from the app's own timestamps; null when unknown. */
function lifeShare(note: NoteRecord): number | null {
  if (note.depositedAt === undefined || note.expiry === undefined) return null;
  const total = note.expiry * 1000 - note.depositedAt;
  return total > 0 ? clamp((Date.now() - note.depositedAt) / total) : null;
}

/** A note that is not accepted yet: it is found again by its deposit receipt, or on the vault. */
function FindDeposit({
  note,
  onChanged,
  setMsg,
}: {
  note: NoteRecord;
  onChanged: () => void;
  setMsg: (m: string) => void;
}) {
  const [hash, setHash] = useState(note.txHash ?? '');
  const [busy, setBusy] = useState(false);
  const lookable = note.kind === 'pending' && note.phase !== 'rejected';
  useEffect(() => {
    setHash(note.txHash ?? '');
  }, [note.id, note.txHash]);

  const settle = (result: Awaited<ReturnType<typeof confirmDeposit>>) =>
    result.status === 'accepted'
      ? 'Found: the deposit was accepted.'
      : result.status === 'rejected'
        ? `Not accepted: ${result.reason}`
        : `Not found yet: ${result.reason}`;

  const lookup = async () => {
    setBusy(true);
    setMsg('');
    try {
      const withHash = hash && hash !== note.txHash ? await attachHash(note.id, hash) : note;
      // An Intents note is funded by intents.near, not by the user: its hash is the destination
      // transaction that 1Click or the Intents explorer shows.
      const result =
        withHash.sender === INTENTS_SIGNER
          ? await confirmIntents(rpc, withHash, withHash.txHash ?? hash, { archival: archivalRpc })
          : await confirmDeposit(rpc, withHash, null, { waitMs: 8000 });
      setMsg(
        result.status === 'booked' ? result.bookings.map(explainBooking).join(' ') : settle(result),
      );
      onChanged();
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // No hash needed: the vault's public note list holds a leaf that only this note's commitment
  // can produce. The scan names no note to the RPC provider.
  const search = async () => {
    setBusy(true);
    setMsg('');
    try {
      const found = await findDeposit(
        rpc,
        note.vault,
        note,
        0,
        (noteId, commitment, amount, expiry) =>
          callWorker({ op: 'noteLeaf', noteId, commitment, amount, expiry }),
      );
      if (found) {
        await acceptFound(note, found);
        setMsg('Found: the deposit was accepted.');
        onChanged();
      } else {
        setMsg('Not found on the vault yet. If the transfer went out, try again in a minute.');
      }
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="nc-pending" data-testid="find-deposit">
      <p className="nc-pending-for">
        For the note{' '}
        <strong>{note.noteId !== undefined ? `#${note.noteId}` : 'with no ID yet'}</strong>,{' '}
        {formatUnits(note.denomination, PROFILE.tokenDecimals)} {PROFILE.tokenSymbol}, payout{' '}
        <code>{note.payout}</code>.
      </p>
      {lookable && (
        <>
          <LookupRow
            hash={hash}
            onHash={setHash}
            onLookup={lookup}
            disabled={busy || hash === ''}
            label="Look up the receipt"
            inputTestId="lookup-hash"
            buttonTestId="lookup"
          />
          <p className="nc-cap nc-pending-alt">
            No hash at hand?{' '}
            <button
              type="button"
              className="nc-link"
              disabled={busy}
              onClick={search}
              data-testid="search-vault"
            >
              Search the vault for this note
            </button>
          </p>
        </>
      )}
    </div>
  );
}

export function Notes({
  onPrivacy,
  onChat,
  onClose,
  onFund,
  shell,
  onChanged,
}: {
  onPrivacy: (noteId: string) => void;
  onChat: () => void;
  onClose: () => void;
  onFund: () => void;
  /** The accepted notes with their live balance, from the app shell. */
  shell: readonly (RowNote & { key: string })[];
  onChanged: () => void;
}) {
  const [activeId, setActiveId] = useState('');
  const [lookupMsg, setLookupMsg] = useState('');
  const [showUnconfirmed, setShowUnconfirmed] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [unlocked, setUnlocked] = useState(isUnlocked());
  const [notes, setNotes] = useState<NoteRecord[]>([]);
  const [pass, setPass] = useState('');
  const [msg, setMsg] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    loadNotes().then(setNotes, () => setError('IndexedDB is not available in this browser.'));
  }, []);
  useEffect(refresh, [refresh]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError('');
    setMsg('');
    try {
      await action();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onBackup = () =>
    run(async () => {
      const bytes = await makeBackupFile(pass);
      download(bytes as BlobPart, backupFileName(), 'application/octet-stream');
      setMsg('Backup downloaded.');
    });

  const onRestore = (file: File | undefined) => {
    if (!file) return;
    run(async () => {
      const result = await callWorker({
        op: 'restoreBackup',
        key: requireKey(),
        bytes: await readFile(file),
        passphrase: pass,
        scope: { network: PROFILE.network, vault: PROFILE.vaultAccount },
      });
      // A note this browser already holds keeps its newer state: a backup is older by definition.
      const { take, skipped } = mergeRestored(await listNotes(), result.records);
      for (const record of take) await putNote(record);
      setMsg(
        `Restored ${take.length} note${take.length === 1 ? '' : 's'}` +
          (skipped.length > 0
            ? `; ${skipped.length} kept the newer state this browser already holds`
            : '') +
          (result.foreign > 0
            ? `; ${result.foreign} belonged to another network or vault and were skipped`
            : '') +
          '.',
      );
      refresh();
    });
  };

  if (!unlocked) return <Unlock onReady={() => setUnlocked(true)} />;

  const byKey = new Map(shell.map((r) => [r.key, r]));
  const entries = notes.map((rec) => ({
    rec,
    row: byKey.get(rec.id),
    done: rec.kind === 'state' && rec.phase === 'accepted',
  }));
  const confirmed = entries.filter((e) => e.done);
  const unconfirmed = entries.filter((e) => !e.done);
  const picked = entries.find((e) => e.rec.id === activeId) ?? confirmed[0] ?? unconfirmed[0];
  // Nothing was sent for these, so dropping the record loses nothing.
  const removable = (e: (typeof entries)[number]) =>
    e.rec.kind === 'pending' && ['prepared', 'backed_up', 'rejected'].includes(e.rec.phase);
  const remove = (list: typeof entries) =>
    run(async () => {
      for (const e of list) await deleteNote(e.rec.id);
      setConfirmClear(false);
      refresh();
      onChanged();
    });
  const lookable = (e: (typeof entries)[number]) =>
    e.rec.kind === 'pending' && e.rec.phase !== 'rejected';
  const target = picked && lookable(picked) ? picked.rec : entries.find(lookable)?.rec;
  const enough = passphraseLength(pass) >= MIN_PASSPHRASE_LENGTH;
  const closed = picked?.row?.status === 'closed';
  const size = (rec: NoteRecord) => Number(BigInt(rec.denomination)) / 10 ** PROFILE.tokenDecimals;

  return (
    <Screen
      testId="notes"
      title="Notes and backup"
      lead="Every note this browser holds. The note secrets are encrypted before they are stored, with a key that cannot be exported."
      illustration={notesIllustration}
      side={
        <>
          {picked && (
            <section className="nc-sum">
              <h2>
                {picked.rec.noteId !== undefined ? `Note #${picked.rec.noteId}` : 'Note, no ID yet'}
              </h2>
              <p className="nc-big">
                <Count value={picked.row ? picked.row.balance : size(picked.rec)} />{' '}
                <small>{PROFILE.tokenSymbol}</small>
              </p>
              <Strip
                n={20}
                solid={picked.row ? clamp(picked.row.balance / picked.row.size) * 20 : 0}
                size="md"
                label="Balance"
              />
              <Facts
                rows={[
                  ['Status', picked.row ? statusText(picked.row) : PHASE_TEXT[picked.rec.phase]],
                  ...(picked.rec.depositedAt !== undefined
                    ? ([['Deposited', formatDate(picked.rec.depositedAt / 1000 + 1)]] as const)
                    : []),
                  ...(picked.row
                    ? ([['Expires', `in ${Math.max(0, 30 - picked.row.day)} days`]] as const)
                    : []),
                  ['Payout account', picked.rec.payout],
                  ...(picked.row
                    ? ([
                        [
                          'Spent',
                          `${(picked.row.size - picked.row.balance).toFixed(2)} ${PROFILE.tokenSymbol}`,
                        ],
                      ] as const)
                    : []),
                ]}
              />
              {!picked.done && (
                <p className="nc-cap" data-testid="unconfirmed-note">
                  No deposit is confirmed for this note, so it has no ID, balance or expiry yet.
                  {removable(picked)
                    ? ' Nothing was sent for it: you can remove it.'
                    : ' A transfer may have gone out: find the deposit before anything else.'}
                </p>
              )}
              {picked.done && (
                <div className="nc-actions">
                  <Button primary disabled={closed} onClick={onChat}>
                    Chat
                  </Button>
                  <Button disabled={closed} onClick={onClose}>
                    Close
                  </Button>
                </div>
              )}
            </section>
          )}
          <section className="nc-sum">
            <h2>Privacy</h2>
            <p className="nc-cap nc-cap-first">
              What anyone can read about this note on chain, beside what the operator saw for each
              lease, with the set size of every lease.
            </p>
            <Button onClick={() => (picked?.done ? onPrivacy(picked.rec.id) : onFund())}>
              {picked?.done ? 'Open the privacy panel' : 'Fund a note first'}
            </Button>
          </section>
        </>
      }
    >
      {import.meta.env.DEV && PROFILE.id === 'mock' && (
        <p className="nc-cap nc-cap-first">
          Mock build only:{' '}
          <button
            type="button"
            className="nc-link"
            data-testid="seed-samples"
            disabled={busy}
            onClick={() =>
              run(async () => {
                await seedSampleNotes();
                refresh();
                onChanged();
              })
            }
          >
            add three sample notes
          </button>{' '}
          (nothing is sent anywhere).
        </p>
      )}
      {notes.length === 0 ? (
        <NoteLedger columns={['Note', 'Balance', 'Its 30 days']}>
          <div className="nc-ledger-empty">
            <Strip n={30} solid={0} size="sm" />
            <p>
              <strong data-testid="no-notes">No notes in this browser.</strong>
              <small>
                Fund one to start chatting. It shows up here with its balance and its days.
              </small>
            </p>
            <Button primary onClick={onFund}>
              Fund a note
            </Button>
          </div>
        </NoteLedger>
      ) : (
        <>
          {confirmed.length > 0 && (
            <NoteLedger columns={['Note', 'Balance', 'Its 30 days']}>
              {confirmed.map((e, i) => {
                const n = e.row;
                return (
                  <div key={e.rec.id} data-testid="note-row" data-phase={e.rec.phase}>
                    <NoteLedgerRow
                      index={i}
                      active={e === picked}
                      pick={{
                        label: `Select note ${e.rec.noteId ?? ''}`,
                        onPick: () => setActiveId(e.rec.id),
                      }}
                      id={{
                        strong: e.rec.noteId !== undefined ? `#${e.rec.noteId}` : '…',
                        small: `${size(e.rec)} ${PROFILE.tokenSymbol}`,
                      }}
                      mid={
                        n ? (
                          <>
                            <strong>
                              <Count value={n.balance} />
                            </strong>
                            <Strip n={10} solid={clamp(n.balance / n.size) * 10} size="sm" />
                          </>
                        ) : (
                          <>
                            <strong>…</strong>
                            <Strip n={10} solid={0} size="sm" />
                          </>
                        )
                      }
                      late={n?.status === 'expiring'}
                      life={
                        n ? (
                          <>
                            <Strip n={30} solid={n.day} size="sm" />
                            <small>{statusText(n)}</small>
                          </>
                        ) : (
                          <small data-testid="note-phase">{PHASE_TEXT[e.rec.phase]}</small>
                        )
                      }
                      actions={
                        <>
                          <Button
                            disabled={n?.status === 'closed'}
                            onClick={() => {
                              setActiveId(e.rec.id);
                              onChat();
                            }}
                          >
                            Chat
                          </Button>
                          <Button
                            disabled={n?.status === 'closed'}
                            onClick={() => {
                              setActiveId(e.rec.id);
                              onClose();
                            }}
                          >
                            Close
                          </Button>
                        </>
                      }
                    />
                  </div>
                );
              })}
            </NoteLedger>
          )}
          {unconfirmed.length > 0 && (
            <details
              className="nc-unconfirmed"
              open={showUnconfirmed || confirmed.length === 0}
              onToggle={(ev) => setShowUnconfirmed(ev.currentTarget.open)}
              data-testid="unconfirmed"
            >
              <summary>
                {unconfirmed.length} unconfirmed {unconfirmed.length === 1 ? 'note' : 'notes'}
                <small>
                  Made in this browser, no deposit seen on chain. Each “Create the secret” leaves
                  one.
                </small>
              </summary>
              {unconfirmed.some(removable) && (
                <div className="nc-unconfirmed-bar">
                  {confirmClear ? (
                    <>
                      <span>
                        Remove the {unconfirmed.filter(removable).length} that were never sent?
                        Their secrets are deleted from this browser.
                      </span>
                      <Button disabled={busy} onClick={() => remove(unconfirmed.filter(removable))}>
                        Remove them
                      </Button>
                      <Button onClick={() => setConfirmClear(false)}>Keep</Button>
                    </>
                  ) : (
                    <Button onClick={() => setConfirmClear(true)}>
                      Remove all never-sent notes
                    </Button>
                  )}
                </div>
              )}
              <ul>
                {unconfirmed.map((e) => (
                  <li
                    key={e.rec.id}
                    data-testid="note-row"
                    data-phase={e.rec.phase}
                    data-active={e === picked}
                  >
                    <button
                      type="button"
                      className="nc-unconfirmed-pick"
                      aria-pressed={e === picked}
                      aria-label="Select this note"
                      onClick={() => setActiveId(e.rec.id)}
                    />
                    <span>
                      <strong>
                        {size(e.rec)} {PROFILE.tokenSymbol}
                      </strong>
                      <small>payout {e.rec.payout}</small>
                    </span>
                    <Chip tone={TONE[e.rec.phase]}>
                      <span data-testid="note-phase">{PHASE_TEXT[e.rec.phase]}</span>
                    </Chip>
                    {removable(e) && (
                      <Button disabled={busy} onClick={() => remove([e])}>
                        Remove
                      </Button>
                    )}
                  </li>
                ))}
              </ul>
            </details>
          )}
        </>
      )}

      <Card title="Backup and restore">
        <PassField
          label={`Backup passphrase (at least ${MIN_PASSPHRASE_LENGTH} characters)`}
          value={pass}
          length={passphraseLength(pass)}
          min={MIN_PASSPHRASE_LENGTH}
          onChange={setPass}
          testId="notes-backup-passphrase"
        />
        <div className="nc-actions">
          {notes.length === 0 ? (
            <Button primary testId="notes-backup-download" onClick={onFund}>
              Fund a note to back up
            </Button>
          ) : (
            <Button
              primary
              testId="notes-backup-download"
              disabled={busy || !enough}
              onClick={onBackup}
            >
              Download a backup
            </Button>
          )}
          <label className="nc-button nc-file" aria-disabled={busy || !enough}>
            Restore from a backup file
            <input
              type="file"
              data-testid="restore-file"
              disabled={busy || !enough}
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = '';
                onRestore(file);
              }}
            />
          </label>
        </div>
        {msg && (
          <p className="nc-msg" data-tone="ok" data-testid="notes-message">
            {msg}
          </p>
        )}
        {error && (
          <p className="nc-msg" data-tone="err" role="alert" data-testid="notes-error">
            {error}
          </p>
        )}
        <p className="nc-cap">
          A backup file is encrypted with {ARGON2_SENTENCE}. Do not keep it in a synced folder:
          whoever opens it can spend the note. Without the File System Access API this page cannot
          write the backup by itself after each change, so download one yourself;{' '}
          <code>nymirad</code> or a Chromium browser backs up automatically. A note this browser
          already holds keeps its newer state: a backup is older by definition.
        </p>
      </Card>
      {(target || lookupMsg) && (
        <Card title="Find a deposit" aside={<Chip>asks the RPC provider</Chip>}>
          <p className="nc-cap nc-cap-first" data-testid="lookup-help">
            If the page closed during the transfer, the note is found again from its deposit
            receipt. Enter the transaction hash from your wallet’s history or the explorer (for a
            note funded from another chain, the NEAR transaction 1Click or the Intents explorer
            shows for your order). The lookup asks the RPC provider, which sees your IP address,
            account and that hash. Searching the vault needs no hash: it reads the vault’s public
            note list, which names none of your notes, but the provider still sees your IP address.
          </p>
          {target && (
            <FindDeposit
              setMsg={setLookupMsg}
              note={target}
              onChanged={() => {
                refresh();
                onChanged();
              }}
            />
          )}
          {lookupMsg && (
            <p className="nc-msg" data-tone="ok" data-testid="lookup-result">
              {lookupMsg}
            </p>
          )}
        </Card>
      )}
    </Screen>
  );
}
