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
  /** The wallet never answered and the chain shows nothing. */
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
 * (a value) or nothing yet (null). A failed read counts as nothing yet.
 */
export async function raceWallet<T>(
  send: Promise<unknown>,
  probe: () => Promise<T | null | undefined>,
  options: RaceOptions = {},
): Promise<Raced<T>> {
  const { pollMs, graceMs, maxMs } = { ...WATCH_DEFAULTS, ...options };
  type Wallet = { ok: true; outcome: unknown } | { ok: false; error: Error };
  const box: { answer?: Wallet } = {};
  const answered = send.then(
    (outcome) => {
      box.answer = { ok: true, outcome };
    },
    (error: unknown) => {
      box.answer = {
        ok: false,
        error: error instanceof Error ? error : new Error(String(error)),
      };
    },
  );
  const read = async () => (await probe().catch(() => null)) ?? null;
  const started = Date.now();
  let graceEnds = 0;
  for (;;) {
    const early = box.answer;
    if (early?.ok) return { by: 'wallet', outcome: early.outcome };
    const found = await read();
    if (found !== null) return { by: 'chain', found };
    const now = Date.now();
    const answer = box.answer;
    if (answer?.ok) return { by: 'wallet', outcome: answer.outcome };
    if (answer) {
      graceEnds ||= now + graceMs;
      if (now >= graceEnds) return { by: 'rejected', error: answer.error };
    }
    if (!answer && Date.now() - started >= maxMs) return { by: 'timeout' };
    await (answer ? sleep(pollMs) : Promise.race([answered, sleep(pollMs)]));
  }
}
