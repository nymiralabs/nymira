// The backup prompt after a lease ends. It lives outside the Chat screen so a retire that happens
// while the user leaves the screen still asks for a backup, on whichever screen they land.
export interface RetireBackup {
  needed: boolean;
  leases: number;
}

type Listener = () => void;

class Store {
  state: RetireBackup | null = null;
  private listeners = new Set<Listener>();

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  };

  set(next: RetireBackup) {
    this.state = next;
    this.listeners.forEach((l) => l());
  }

  clear() {
    this.state = null;
    this.listeners.forEach((l) => l());
  }
}

export const retireBackup = new Store();
