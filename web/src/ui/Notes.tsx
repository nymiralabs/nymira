import { useCallback, useEffect, useState } from 'react';
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

function NoteRow({
  note,
  onChanged,
  onPrivacy,
}: {
  note: NoteRecord;
  onChanged: () => void;
  onPrivacy: (noteId: string) => void;
}) {
  const [hash, setHash] = useState(note.txHash ?? '');
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);
  const lookable = note.kind === 'pending' && note.phase !== 'rejected';

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
    <li data-testid="note-row" data-phase={note.phase}>
      <strong>
        {formatUnits(note.denomination, PROFILE.tokenDecimals)} {PROFILE.tokenSymbol}
      </strong>{' '}
      <span data-testid="note-phase">{PHASE_TEXT[note.phase]}</span>
      {note.noteId !== undefined && <> · note #{note.noteId}</>}
      {note.expiry !== undefined && <> · expires {formatDate(note.expiry)}</>}
      <br />
      <small>payout {note.payout}</small>
      {note.phase === 'accepted' && note.kind === 'state' && (
        <>
          {' '}
          <button type="button" data-testid="notes-privacy" onClick={() => onPrivacy(note.id)}>
            Privacy panel
          </button>
        </>
      )}
      {lookable && (
        <div>
          <p className="hint">
            If the page closed during the transfer, the note is found again from its deposit
            receipt. Enter the transaction hash from your wallet’s history or the explorer (for a
            note funded from another chain, the NEAR transaction 1Click or the Intents explorer
            shows for your order). The lookup asks the RPC provider, which sees your IP address,
            account and that hash.
          </p>
          <input
            type="text"
            aria-label="Transaction hash"
            value={hash}
            placeholder="transaction hash"
            data-testid="lookup-hash"
            onChange={(e) => setHash(e.target.value.trim())}
          />
          <button
            type="button"
            disabled={busy || hash === ''}
            data-testid="lookup"
            onClick={lookup}
          >
            Look up the deposit receipt
          </button>
        </div>
      )}
      {msg && <p data-testid="lookup-result">{msg}</p>}
    </li>
  );
}

export function Notes({ onPrivacy }: { onPrivacy: (noteId: string) => void }) {
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

  return (
    <section data-testid="notes">
      <h2>Notes and backup</h2>
      {notes.length === 0 ? (
        <p data-testid="no-notes">No notes in this browser.</p>
      ) : (
        <ul className="notes">
          {notes.map((n) => (
            <NoteRow key={n.id} note={n} onChanged={refresh} onPrivacy={onPrivacy} />
          ))}
        </ul>
      )}

      <h3>Backup and restore</h3>
      <p className="hint">
        A backup file is encrypted with {ARGON2_SENTENCE}. Do not keep it in a synced folder:
        whoever opens it can spend the note. Without the File System Access API this page cannot
        write the backup by itself after each change, so download one yourself; <code>nymirad</code>{' '}
        or a Chromium browser backs up automatically.
      </p>
      <label>
        Backup passphrase (at least {MIN_PASSPHRASE_LENGTH} characters)
        <input
          type="password"
          autoComplete="off"
          value={pass}
          data-testid="notes-backup-passphrase"
          onChange={(e) => setPass(e.target.value)}
        />
      </label>
      <button
        type="button"
        data-testid="notes-backup-download"
        disabled={busy || passphraseLength(pass) < MIN_PASSPHRASE_LENGTH || notes.length === 0}
        onClick={onBackup}
      >
        Download a backup
      </button>
      <label>
        Restore from a backup file
        <input
          type="file"
          data-testid="restore-file"
          disabled={busy || passphraseLength(pass) < MIN_PASSPHRASE_LENGTH}
          onChange={(e) => {
            const file = e.target.files?.[0];
            e.target.value = '';
            onRestore(file);
          }}
        />
      </label>
      {msg && (
        <p className="ok" data-testid="notes-message">
          {msg}
        </p>
      )}
      {error && (
        <p className="error" role="alert" data-testid="notes-error">
          {error}
        </p>
      )}
    </section>
  );
}
