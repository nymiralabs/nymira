// A conversation that is still open while the person is on another screen: what the strip under the
// header needs to say, and the two things it can do (go back, end it now).
export interface LiveChat {
  noteId: number;
  used: string;
  bound: string;
  /** When the person left the Chat screen, in ms; the conversation ends `IDLE_END_MS` later. */
  awaySince: number | null;
}

/** How long a conversation stays open while the person is on another screen. */
export const IDLE_END_MS = 10 * 60_000;

type Listener = () => void;

class Store {
  state: LiveChat | null = null;
  /** Ends the open conversation (set by the Chat screen while one is open). */
  end: (() => void) | null = null;
  private listeners = new Set<Listener>();

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  };

  set(next: LiveChat | null, end: (() => void) | null = this.end) {
    this.state = next;
    this.end = end;
    this.listeners.forEach((l) => l());
  }
}

export const liveChat = new Store();
