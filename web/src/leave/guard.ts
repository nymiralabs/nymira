// The expiry guard, as `nymirad` runs it (`crates/nymirad/src/expiry.rs`), with the same numbers.
// A note that reaches its expiry goes to the treasury. Counting back from the expiry:
//   14, 7 and 2 days  a notice (once per level per note);
//   5 days            no new lease on the note, and while the app is open a mutual close starts;
//   3 days            if the close did not go through, `request_exit` starts.
// "Starts" means the proofs are made and the wallet is asked to sign: the app sends nothing the
// wallet has not signed. The guard is on by default and can be switched off. Closing at the end of
// a session is a separate switch and is off by default: every close shows the note ID and the
// total spend on chain.
import type { NoteRecord } from '../store/types';

export const DAY = 86_400;
/** No new lease when this many days or fewer are left; a mutual close is tried. */
export const CLOSE_DAYS = 5;
/** `request_exit` is filed when this many days or fewer are left and the close did not go through. */
export const EXIT_DAYS = 3;
export const NOTICE_DAYS = [14, 7, 2] as const;
/** A failed close or exit is tried again after this many seconds. */
export const RETRY_SECS = 3_600;

export interface GuardSettings {
  /** The lease cut-off, the automatic close and exit, the notices. */
  enabled: boolean;
  /** Start a close when a conversation ends. Off by default. */
  closeAtSessionEnd: boolean;
}

export const DEFAULT_SETTINGS: GuardSettings = { enabled: true, closeAtSessionEnd: false };

/** What the guard remembers about one note (public values only, carried in backups). */
export interface GuardMemory {
  /** The lowest notice level shown so far (14, 7 or 2). */
  notice?: number;
  /** A mutual close was tried and failed. */
  closeFailed?: boolean;
  /** Unix seconds of the last automatic try. */
  lastTry?: number;
}

export const remaining = (expiry: number, now: number): number => Math.max(0, expiry - now);

/** No new lease: the note is within five days of its expiry (or past it). */
export const leaseBlocked = (expiry: number, now: number): boolean =>
  remaining(expiry, now) <= CLOSE_DAYS * DAY;

export type Due = 'nothing' | 'close' | 'exit';

export function due(expiry: number, now: number): Due {
  const left = remaining(expiry, now);
  if (left <= EXIT_DAYS * DAY) return 'exit';
  if (left <= CLOSE_DAYS * DAY) return 'close';
  return 'nothing';
}

/** The notice level (14, 7 or 2) that applies: the smallest not yet passed. `undefined` above 14 days. */
export function noticeLevel(expiry: number, now: number): number | undefined {
  const left = remaining(expiry, now);
  const levels = NOTICE_DAYS.filter((d) => left <= d * DAY);
  return levels.length > 0 ? Math.min(...levels) : undefined;
}

export function noticeText(noteId: number | undefined, level: number, leftSecs: number): string {
  const left = leftSecs >= DAY ? `${Math.floor(leftSecs / DAY)} days` : 'less than a day';
  const which = noteId === undefined ? 'a note' : `note #${noteId}`;
  return `${which} expires in ${left} (${level}-day notice). At 5 days this app stops opening leases on it and, while it is open, starts a close; at 3 days it starts an exit. Each waits for your wallet signature.`;
}

export type Action =
  | { kind: 'notice'; note: NoteRecord; level: number; text: string }
  | { kind: 'close'; note: NoteRecord }
  | { kind: 'exit'; note: NoteRecord };

/** A note the guard looks at: an accepted note that has not started to leave. */
export const watched = (n: NoteRecord): boolean =>
  n.kind === 'state' && n.phase === 'accepted' && n.expiry !== undefined && !n.leave;

/** A note whose exit was started but never filed (the send failed) is finished by the guard too. */
export const stuckExit = (n: NoteRecord): boolean =>
  n.kind === 'state' &&
  n.expiry !== undefined &&
  n.leave?.state === 'exiting' &&
  n.leave.deadline === undefined;

/**
 * What the guard would do now. Notices are returned when a lower level than the one shown applies
 * (the caller records it). Closes and exits wait `RETRY_SECS` after a try. With the guard off,
 * nothing is returned.
 */
export function plan(notes: NoteRecord[], now: number, settings: GuardSettings): Action[] {
  if (!settings.enabled) return [];
  const actions: Action[] = [];
  for (const note of notes) {
    const expiry = note.expiry;
    if (expiry === undefined) continue;
    const open = watched(note);
    const stuck = stuckExit(note);
    if (!open && !stuck) continue;
    const memory = note.guard ?? {};
    if (open) {
      const level = noticeLevel(expiry, now);
      if (level !== undefined && (memory.notice === undefined || level < memory.notice)) {
        actions.push({
          kind: 'notice',
          note,
          level,
          text: noticeText(note.noteId, level, remaining(expiry, now)),
        });
      }
    }
    const waiting = memory.lastTry !== undefined && now - memory.lastTry < RETRY_SECS;
    if (waiting) continue;
    const what = due(expiry, now);
    if (what === 'close' && open) actions.push({ kind: 'close', note });
    else if (what === 'exit') {
      // A close that was never tried still comes first, as in `nymirad`.
      if (open && !memory.closeFailed) actions.push({ kind: 'close', note });
      else actions.push({ kind: 'exit', note });
    }
  }
  return actions;
}
