// The pinned attested models and their prices (nano-USD per token), as the live list gave them on
// 2026-11-02. They match `nymira-operator`'s default `ProxyConfig`; a test compares the two. The
// operator refuses any other model id, and so does the app: no aliasing.
import type { ModelSpec } from './charge';

export const MODELS: readonly ModelSpec[] = [
  { id: 'z-ai/glm-5.3-flash', inputNano: 80n, outputNano: 500n },
  { id: 'Qwen/Qwen3.6-35B-A3B-FP8', inputNano: 170n, outputNano: 1100n },
  { id: 'Qwen/Qwen3.8-27B', inputNano: 40n, outputNano: 1350n },
  { id: 'Qwen/Qwen3-VL-30B-A3B-Instruct', inputNano: 150n, outputNano: 550n },
];

export const DEFAULT_MODEL = 'z-ai/glm-5.3-flash';

/** The 0.10 USDC class is the default; the "long job" option picks the 0.50 USDC class. */
export const DEFAULT_BOUND = 100_000n;
export const LONG_JOB_BOUND = 500_000n;

export function modelById(id: string): ModelSpec | undefined {
  return MODELS.find((m) => m.id === id);
}

/** The class for a request: the default one, or the 0.50 class when the user asked for it. */
export function chooseBound(classes: bigint[], longJob: boolean): bigint | null {
  const want = longJob ? LONG_JOB_BOUND : DEFAULT_BOUND;
  return classes.includes(want) ? want : null;
}
