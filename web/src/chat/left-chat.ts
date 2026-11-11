// What happened to a conversation that was open when the person left the Chat screen: the lease is
// ended in the background, and the person is told what it cost, on whichever screen they landed.
export type LeftChat =
  { ok: true; charge: string; balance: string } | { ok: false; reason: string };

type Listener = () => void;

class Store {
  state: LeftChat | null = null;
  private listeners = new Set<Listener>();

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  };

  set(next: LeftChat) {
    this.state = next;
    this.listeners.forEach((l) => l());
  }

  clear() {
    this.state = null;
    this.listeners.forEach((l) => l());
  }
}

export const leftChat = new Store();
