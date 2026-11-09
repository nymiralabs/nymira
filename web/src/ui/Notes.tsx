import { useCallback, useEffect, useState } from 'react';
import './notes.css';
import notesIllustration from '../marketing/assets/app-notes.webp';
import { Button, Card, Chip, Facts, Screen, Strip, clamp } from '../marketing/kit';
import { LookupRow, NoteLedger, NoteLedgerRow, PassField } from '../marketing/kit-notes';
import { PROFILE } from '../profile';
import { backupFileName } from '../backup/format';
import { MIN_PASSPHRASE_LENGTH, passphraseLength } from '../crypto/kdf';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { attachHash, confirmDeposit, loadNotes, makeBackupFile } from '../funding/flow';
import { INTENTS_SIGNER, confirmIntents, explainBooking } from '../funding/intents';
import { listNotes, putNote } from '../store/db';
import { mergeRestored } from '../store/merge';
import type { NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
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

function NoteRow({
  note,
  index,
  active,
  onPick,
  onChanged,
  onPrivacy,
  onChat,
  onClose,
}: {
  note: NoteRecord;
  index: number;
  active: boolean;
  onPick: () => void;
  onChanged: () => void;
  onPrivacy: (noteId: string) => void;
  onChat: () => void;
  onClose: () => void;
}) {
  const [hash, setHash] = useState(note.txHash ?? '');
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);
  const lookable = note.kind === 'pending' && note.phase !== 'rejected';
  const share = lifeShare(note);

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
        result.status === 'accepted'
          ? 'Found: the deposit was accepted.'
          : result.status === 'booked'
            ? result.bookings.map(explainBooking).join(' ')
            : result.status === 'rejected'
              ? `Not accepted: ${result.reason}`
              : `Not found yet: ${result.reason}`,
      );
      onChanged();
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="nc-ledger-item" data-testid="note-row" data-phase={note.phase}>
      <NoteLedgerRow
        index={index}
        active={active}
        pick={{ label: `Select note ${note.noteId ?? note.id}`, onPick }}
        id={{
          strong: note.noteId !== undefined ? `#${note.noteId}` : 'no id yet',
          small: `${formatUnits(note.denomination, PROFILE.tokenDecimals)} ${PROFILE.tokenSymbol}`,
        }}
        mid={<small>payout {note.payout}</small>}
        life={
          <>
            <Chip tone={TONE[note.phase]}>
              <span data-testid="note-phase">{PHASE_TEXT[note.phase]}</span>
            </Chip>
            {share !== null && <Strip n={30} solid={share * 30} size="sm" />}
            {note.expiry !== undefined && <small>expires {formatDate(note.expiry)}</small>}
          </>
        }
        actions={
          <>
            {note.phase === 'accepted' && note.kind === 'state' && (
              <Button testId="notes-privacy" onClick={() => onPrivacy(note.id)}>
                Privacy panel
              </Button>
            )}
            <Button onClick={onChat}>Chat</Button>
            <Button onClick={onClose}>Close</Button>
          </>
        }
      />
      {(lookable || msg) && (
        <div className="nc-ledger-more">
          {lookable && (
            <>
              <p className="nc-cap nc-cap-first">
                If the page closed during the transfer, the note is found again from its deposit
                receipt. Enter the transaction hash from your wallet’s history or the explorer (for
                a note funded from another chain, the NEAR transaction 1Click or the Intents
                explorer shows for your order). The lookup asks the RPC provider, which sees your IP
                address, account and that hash.
              </p>
              <LookupRow
                hash={hash}
                onHash={setHash}
                onLookup={lookup}
                disabled={busy || hash === ''}
                label="Look up the deposit receipt"
                inputTestId="lookup-hash"
                buttonTestId="lookup"
              />
            </>
          )}
          {msg && (
            <p className="nc-msg" data-tone="ok" data-testid="lookup-result">
              {msg}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

export function Notes({
  onPrivacy,
  onChat,
  onClose,
  onFund,
}: {
  onPrivacy: (noteId: string) => void;
  onChat: () => void;
  onClose: () => void;
  onFund: () => void;
}) {
  const [active, setActive] = useState(0);
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

  const sel = notes[Math.min(active, notes.length - 1)];
  const enough = passphraseLength(pass) >= MIN_PASSPHRASE_LENGTH;
  const openPrivacy = () =>
    sel && sel.phase === 'accepted' && sel.kind === 'state' ? onPrivacy(sel.id) : onFund();

  return (
    <Screen
      testId="notes"
      title="Notes and backup"
      lead="Every note this browser holds. The note secrets are encrypted before they are stored, with a key that cannot be exported."
      illustration={notesIllustration}
      side={
        <>
          {sel && (
            <section className="nc-sum">
              <h2>{sel.noteId !== undefined ? `Note #${sel.noteId}` : 'Note'}</h2>
              <p className="nc-big">
                {formatUnits(sel.denomination, PROFILE.tokenDecimals)}{' '}
                <small>{PROFILE.tokenSymbol}</small>
              </p>
              <Facts
                rows={[
                  ['Status', PHASE_TEXT[sel.phase]],
                  ...(sel.depositedAt !== undefined
                    ? ([['Deposited', formatDate(sel.depositedAt / 1000 + 1)]] as const)
                    : []),
                  ...(sel.expiry !== undefined
                    ? ([['Expires', formatDate(sel.expiry)]] as const)
                    : []),
                  ['Payout account', sel.payout],
                ]}
              />
              <div className="nc-actions">
                <Button primary onClick={onChat}>
                  Chat
                </Button>
                <Button onClick={onClose}>Close</Button>
              </div>
            </section>
          )}
          <section className="nc-sum">
            <h2>Privacy</h2>
            <p className="nc-cap nc-cap-first">
              What anyone can read about this note on chain, beside what the operator saw for each
              lease, with the set size of every lease.
            </p>
            <Button onClick={openPrivacy}>
              {sel && sel.phase === 'accepted' && sel.kind === 'state'
                ? 'Open the privacy panel'
                : 'Fund a note first'}
            </Button>
          </section>
        </>
      }
    >
      {notes.length === 0 ? (
        <Card title="No notes">
          <p className="nc-cap nc-cap-first" data-testid="no-notes">
            No notes in this browser.
          </p>
          <Button primary onClick={onFund}>
            Fund a note
          </Button>
        </Card>
      ) : (
        <NoteLedger columns={['Note', 'Payout', 'Its life']}>
          {notes.map((n, i) => (
            <NoteRow
              key={n.id}
              note={n}
              index={i}
              active={sel === n}
              onPick={() => setActive(i)}
              onChanged={refresh}
              onPrivacy={onPrivacy}
              onChat={onChat}
              onClose={onClose}
            />
          ))}
        </NoteLedger>
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
    </Screen>
  );
}
