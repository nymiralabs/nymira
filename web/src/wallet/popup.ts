// Popup hand-off for wallets that approve in a window (Meteor). A browser lets a page open a window
// only while the user's click is fresh. NEAR Connect asks the host frame to open the wallet's
// window only after the wallet's own executor has done async work, by which time the click has
// expired and the browser blocks the window. So the frame opens a blank window while the click is
// still fresh, keeps the handle, and gives it to the next `window.open` call, which navigates it.
// The caller gets the same handle back, so NEAR Connect's `message` listener and its `closed`
// polling work as before. The frame's sandbox is not touched.

export interface PopupHost {
  open: (url?: string | URL, target?: string, features?: string) => Window | null;
}

/** A small window, like the ones wallets ask for; the wallet's own features cannot be known yet. */
const FEATURES = 'popup=yes,width=480,height=720';
/** Longest an armed window stays open without a request using it. */
export const ARM_MS = 120_000;

export class PopupHandoff {
  private pre: Window | null = null;
  private taken = false;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    host: PopupHost,
    private readonly nativeOpen: PopupHost['open'] = host.open.bind(host),
  ) {
    host.open = (url, target, features) => {
      const pre = this.pre;
      if (pre && !this.taken && !pre.closed && url !== undefined && String(url) !== 'about:blank') {
        this.taken = true;
        pre.location.href = String(url);
        return pre;
      }
      return this.nativeOpen(url, target, features);
    };
  }

  /** Opens the blank window now. Call it while the user's click is fresh. */
  arm(lifetimeMs: number = ARM_MS): boolean {
    if (this.pre && !this.taken && !this.pre.closed) return true;
    this.release();
    // `nativeOpen`: the override must not hand the new blank window to itself.
    this.pre = this.nativeOpen('about:blank', '_blank', FEATURES);
    this.taken = false;
    if (this.pre) this.timer = setTimeout(() => this.release(), lifetimeMs);
    return this.pre !== null;
  }

  /** Ends a request: closes a window nobody used. A window the wallet took is the wallet's. */
  release(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
    if (this.pre && !this.taken && !this.pre.closed) this.pre.close();
    this.pre = null;
    this.taken = false;
  }
}
