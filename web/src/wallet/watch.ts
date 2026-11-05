// A wallet may never answer: Intear showed "Couldn't preview these transactions" (its own archival
// RPC answered 400), the user approved, and the window stayed open although the transaction landed.
// Closing that window then reports "User rejected the transaction". So the app does not depend on
// the wallet returning: while it waits it reads the chain for the effect of what it sent, and it
// calls a request rejected only when the chain shows nothing after a grace period.

export type Raced<T> =
  /** The wallet answered. */
  | { by: 'wallet'; outcome: unknown }
  /** The chain shows the effect; whatever the wallet says later is ignored. */
  | { by: 'chain'; found: T }
  /** The wallet refused (or failed) and the chain shows nothing after the grace period. */
  | { by: 'rejected'; error: Error }
  /** The result remains unconfirmed when the overall deadline expires. */
  | { by: 'timeout' };

export interface RaceOptions {
  /** Between two reads of the chain. */
  pollMs?: number;
  /** After a refusal: how long a closed window may still have sent. */
  graceMs?: number;
  /** After this long without an answer and without an effect, give up waiting. */
  maxMs?: number;
}

export const WATCH_DEFAULTS = { pollMs: 3000, graceMs: 20_000, maxMs: 180_000 };

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/**
 * `send` is the wallet's answer; `probe` reads the chain and returns what it shows of the request
 * (a value) or nothing yet (null). A failed read leaves the result unknown.
 */
export async function raceWallet<T>(
  send: Promise<unknown>,
  probe: () => Promise<T | null | undefined>,
  options: RaceOptions = {},
): Promise<Raced<T>> {
  const { pollMs, graceMs, maxMs } = { ...WATCH_DEFAULTS, ...options };
  let rejection: { error: Error; graceEnds: number } | undefined;
  const success = new Promise<Raced<T>>((resolve) => {
    send.then(
      (outcome) => resolve({ by: 'wallet', outcome }),
      (error: unknown) => {
        rejection = {
          error: error instanceof Error ? error : new Error(String(error)),
          graceEnds: Date.now() + graceMs,
        };
      },
    );
  });
  let timer: ReturnType<typeof setTimeout>;
  const deadline = new Promise<Raced<T>>((resolve) => {
    timer = setTimeout(() => resolve({ by: 'timeout' }), maxMs);
  });
  try {
    for (;;) {
      const probeStarted = Date.now();
      const read = probe().then(
        (found) => ({ by: 'probe' as const, found: found ?? null, readable: true }),
        () => ({ by: 'probe' as const, found: null, readable: false }),
      );
      const result = await Promise.race([read, success, deadline]);
      if (result.by !== 'probe') return result;
      if (result.found !== null) return { by: 'chain', found: result.found };
      if (result.readable && rejection && probeStarted >= rejection.graceEnds) {
        return { by: 'rejected', error: rejection.error };
      }
      const wake = await Promise.race([sleep(pollMs).then(() => null), success, deadline]);
      if (wake) return wake;
    }
  } finally {
    clearTimeout(timer!);
  }
}
