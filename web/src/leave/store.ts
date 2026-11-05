// What the Close screen and the expiry guard share: the notes with what the worker says about
// them, the one running close or exit, the guard's switches, the operator's emergency notice and
// the notices the guard showed. A plain external store (like `walletClient`), so the screens and
// the page-wide banner see the same thing. Nothing here holds a secret: the worker reads the notes.
import { PROFILE } from '../profile';
import { isUnlocked, requireKey } from '../crypto/wrap';
import { loadNotes } from '../funding/flow';
import { leaveDeps } from '../chat/runtime';
import { OperatorClient } from '../chat/operator';
import type { NoteView } from '../note/lease';
import { getNote, putNote } from '../store/db';
import { noteAad, type NoteRecord } from '../store/types';
import { walletClient } from '../wallet/client';
import { callWorker } from '../worker/client';
import { DEFAULT_SETTINGS, due, plan, RETRY_SECS, watched, type GuardSettings } from './guard';
import {
  checkClosed,
  claimPayout,
  closeNote,
  describe,
  finalizeExit,
  pendingFor,
  refreshExit,
  requestExit,
  type LeaveDeps,
} from './flow';
import { loadGuardSettings, saveGuardSettings } from './settings';

export interface Row {
  record: NoteRecord;
  view: NoteView;
}

export interface LeaveState {
  rows: Row[];
  busy: { id: string; text: string } | null;
  error: string;
  message: string;
  /** The operator's emergency notice, when it shows one. */
  emergency: string | null;
  settings: GuardSettings;
  /** Notes that were just closed or finalized and have no backup since. */
  backupFor: string[];
  /** The latest pending payout read for a note, base units. */
  pending: Record<string, string>;
  version: number;
}

const EMERGENCY_EVERY_MS = 300_000;

class LeaveStore {
  state: LeaveState = {
    rows: [],
    busy: null,
    error: '',
    message: '',
    emergency: null,
    settings: { ...DEFAULT_SETTINGS },
    backupFor: [],
    pending: {},
    version: 0,
  };
  private listeners = new Set<() => void>();
  private emergencyAt = 0;
  private ticking = false;
  /** Overridable in tests of the store. */
  now: () => number = Date.now;

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  getSnapshot = () => this.state.version;

  private set(patch: Partial<LeaveState>) {
    this.state = { ...this.state, ...patch, version: this.state.version + 1 };
    for (const l of this.listeners) l();
  }

  private deps(): Promise<LeaveDeps> {
    return leaveDeps(requireKey);
  }

  private nowSecs = () => Math.floor(this.now() / 1000);

  /** Reads the notes of this vault and what the worker says about each. */
  async refresh(): Promise<Row[]> {
    if (!isUnlocked()) {
      this.set({ rows: [] });
      return [];
    }
    this.state = { ...this.state, settings: await loadGuardSettings() };
    const key = requireKey();
    const records = (await loadNotes()).filter((n) => n.kind === 'state' && n.phase === 'accepted');
    const rows = await Promise.all(
      records.map(async (record) => ({
        record,
        view: await callWorker({
          op: 'readNote',
          key,
          aad: noteAad(record.network, record.vault, record.id),
          sealed: record.sealed,
        }),
      })),
    );
    this.set({ rows });
    return rows;
  }

  async setSettings(settings: GuardSettings) {
    await saveGuardSettings(settings);
    this.set({ settings });
  }

  clearMessages() {
    this.set({ error: '', message: '' });
  }

  dismissBackup(id: string) {
    this.set({ backupFor: this.state.backupFor.filter((x) => x !== id) });
  }

  /** Marks a backup as due after a close or finalized exit; the screen prompts for it. */
  private backupDue(id: string) {
    if (!this.state.backupFor.includes(id)) this.set({ backupFor: [...this.state.backupFor, id] });
  }

  // ---- the operator's emergency notice ----

  /** Reads the operator's status for an emergency notice, at most every five minutes. */
  async checkEmergency(force = false): Promise<void> {
    if (!isUnlocked() || !PROFILE.operatorUrl) return;
    if (!force && this.now() - this.emergencyAt < EMERGENCY_EVERY_MS) return;
    // Nothing to protect without an open note, and no reason to call the operator.
    if (!this.state.rows.some((r) => !r.record.leave)) return;
    this.emergencyAt = this.now();
    try {
      const status = await new OperatorClient(PROFILE.operatorUrl).status();
      const notice = status.emergency_notice;
      this.set({ emergency: typeof notice === 'string' && notice !== '' ? notice : null });
    } catch {
      // An unreachable operator is not an all-clear, and not an alarm: keep what was known.
    }
  }

  /** What to tell the user about an emergency: leases are stopped, an exit is the way out. */
  emergencyAdvice(): string | null {
    const notice = this.state.emergency;
    if (notice === null) return null;
    const ids = this.state.rows.filter((r) => !r.record.leave).map((r) => `#${r.view.noteId}`);
    const exits =
      ids.length === 0
        ? 'No note is open.'
        : `To get the money out, use Request exit on the Close screen for each open note (${ids.join(', ')}).`;
    return `The operator reports an emergency: ${notice}. No new lease is opened. Consider leaving: the operator’s keys may be exposed. ${exits}`;
  }

  // ---- actions ----

  private async run<T>(
    note: NoteRecord,
    text: string,
    action: (deps: LeaveDeps, step: (t: string) => void, fresh: NoteRecord) => Promise<T>,
    usesWallet = false,
  ): Promise<T | undefined> {
    if (this.state.busy) {
      this.set({ error: 'Another close or exit is running. Wait for it to finish.' });
      return undefined;
    }
    // Before any await: the click is fresh, and a wallet that opens a window needs it (popup.ts).
    if (usesWallet) walletClient.arm();
    this.set({ busy: { id: note.id, text }, error: '', message: '' });
    try {
      const deps = await this.deps();
      const fresh = (await getNote(note.id)) ?? note;
      return await action(deps, (t) => this.set({ busy: { id: note.id, text: t } }), fresh);
    } catch (e) {
      this.set({ error: describe(e) });
      return undefined;
    } finally {
      if (usesWallet) walletClient.release();
      this.set({ busy: null });
      await this.refresh().catch(() => undefined);
    }
  }

  async close(note: NoteRecord, via: 'wallet' | 'operator') {
    const result = await this.run(
      note,
      'Starting the close…',
      (d, step, n) => closeNote(d, n, { via, step }),
      via === 'wallet',
    );
    if (result) {
      if (result.confirmed) {
        this.backupDue(note.id);
        this.set({
          message: `Note #${note.noteId} is closed. The vault shows it closed and pays ${note.payout}.`,
        });
      } else {
        this.set({
          message:
            'The close was sent through the operator, but the vault does not show the note closed yet. Check again in a moment.',
        });
      }
    }
    return result;
  }

  async recheckClosed(note: NoteRecord) {
    const result = await this.run(note, 'Asking the vault…', async (d, _s, n) => checkClosed(d, n));
    if (result?.leave?.state === 'closed') {
      this.backupDue(note.id);
      this.set({ message: `Note #${note.noteId} is closed.` });
    } else if (result) {
      this.set({ message: 'The vault does not show the note closed yet.' });
    }
  }

  async exit(note: NoteRecord, settle?: () => Promise<void>) {
    const result = await this.run(
      note,
      'Starting the exit…',
      async (d, step, n) => {
        if (settle) {
          step('Settling the unsettled lease first, if the operator can…');
          await settle().catch(() => undefined);
        }
        return requestExit(d, (await getNote(note.id)) ?? n, { step });
      },
      true,
    );
    if (result) {
      this.backupDue(note.id);
      this.set({
        message: `The exit request for note #${note.noteId} is filed. It concedes ${result.conceded} base units. finalize_exit works after ${new Date(result.deadline * 1000).toISOString()}.`,
      });
    }
    return result;
  }

  async refreshExitState(note: NoteRecord) {
    await this.run(note, 'Asking the vault…', (d, _s, n) => refreshExit(d, n));
  }

  async finalize(note: NoteRecord) {
    const result = await this.run(
      note,
      'Starting finalize_exit…',
      (d, step, n) => finalizeExit(d, n, { step }),
      true,
    );
    if (result) {
      this.backupDue(note.id);
      this.set({
        message: `The exit of note #${note.noteId} is finalized. The vault pays ${note.payout}; if a payout could not be sent it waits for “Claim payout”.`,
      });
    }
    return result;
  }

  /** Reads the payout account's pending payout. Shows the account to the RPC provider. */
  async readPending(note: NoteRecord) {
    const amount = await this.run(note, 'Reading the pending payout…', (d, _s, n) =>
      pendingFor(d, n),
    );
    if (amount !== undefined) {
      this.set({ pending: { ...this.state.pending, [note.id]: amount.toString() } });
    }
  }

  async claim(note: NoteRecord) {
    const result = await this.run(
      note,
      'Starting claim_payout…',
      (d, step, n) => claimPayout(d, n, { step }),
      true,
    );
    if (result) {
      this.set({
        message: `claim_payout was sent for ${note.payout}: ${result.amount} base units.`,
        pending: { ...this.state.pending, [note.id]: '0' },
      });
    }
    return result;
  }

  // ---- the expiry guard ----

  private async patchGuard(note: NoteRecord, patch: NonNullable<NoteRecord['guard']>) {
    const next = { ...note, guard: { ...note.guard, ...patch }, updatedAt: this.now() };
    await putNote(next);
    return next;
  }

  /**
   * One guard pass (`nymirad`'s `guard_tick`): notices, then a close at 5 days, then an exit at 3
   * days when the close did not go through. A close or exit "starts": proofs are made and the
   * wallet is asked; nothing is sent that the wallet did not sign. Needs an unlocked app and a
   * connected wallet; without one it says what is waiting.
   */
  async tick(): Promise<void> {
    if (this.ticking || !isUnlocked()) return;
    this.ticking = true;
    try {
      const rows = await this.refresh();
      await this.checkEmergency();
      const now = this.nowSecs();
      const actions = plan(
        rows.map((r) => r.record),
        now,
        this.state.settings,
      );
      for (const action of actions) {
        if (action.kind === 'notice') {
          // Remembered once per level, so a later pass does not repeat it (the banner shows it).
          await this.patchGuard(action.note, { notice: action.level });
          continue;
        }
        if (this.state.busy) continue;
        if (!walletClient.connected) {
          this.set({
            message: `Note #${action.note.noteId} needs a ${action.kind}: connect your wallet (the Fund or Close screen) and the app starts it.`,
          });
          continue;
        }
        // Recorded before the try: a refused signature or a failure waits an hour (`RETRY_SECS`).
        const marked = await this.patchGuard(action.note, { lastTry: now });
        await this.refresh();
        const expiry = marked.expiry!;
        if (action.kind === 'close') {
          const result = await this.close(marked, 'wallet');
          if (!result?.confirmed) {
            const failed = await this.patchGuard(marked, { closeFailed: true });
            if (due(expiry, now) === 'exit') await this.exit(failed);
          }
        } else {
          await this.exit(marked);
        }
      }
    } finally {
      this.ticking = false;
    }
  }

  /** Whether a note is a candidate for the guard now (for the banner's wording). */
  watching(note: NoteRecord): boolean {
    return this.state.settings.enabled && watched(note);
  }

  /** The seconds the guard waits between tries, for the screen. */
  readonly retrySecs = RETRY_SECS;
}

export const leaveStore = new LeaveStore();
