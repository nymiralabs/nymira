// The page's handle on the note worker. One worker per page, created on first use.
import workerUrl from './note.worker.ts?worker&url';
import type { ResultOf, WorkerRequest, WorkerResponse } from './protocol';

// The app's policy has `require-trusted-types-for 'script'`, and the Worker constructor is one of
// the sinks: it takes a TrustedScriptURL. The one policy this app creates only accepts a URL of
// its own origin, and the policy name is the only one the CSP `trusted-types` list allows.
interface TrustedPolicyFactory {
  createPolicy(
    name: string,
    rules: { createScriptURL(input: string): string },
  ): { createScriptURL(input: string): unknown };
}

function scriptUrl(): string {
  const factory = (globalThis as { trustedTypes?: TrustedPolicyFactory }).trustedTypes;
  if (!factory) return workerUrl;
  const policy = factory.createPolicy('nymira-worker', {
    createScriptURL(input) {
      const url = new URL(input, location.href);
      if (url.origin !== location.origin) throw new Error('a worker must come from this origin');
      return url.href;
    },
  });
  return policy.createScriptURL(workerUrl) as string;
}

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void };

let worker: Worker | null = null;
let counter = 0;
const pending = new Map<number, Pending>();

function ensureWorker(): Worker {
  if (worker) return worker;
  worker = new Worker(scriptUrl(), { type: 'module' });
  worker.onmessage = (event: MessageEvent<WorkerResponse>) => {
    const entry = pending.get(event.data.id);
    if (!entry) return;
    pending.delete(event.data.id);
    if (event.data.ok) entry.resolve(event.data.result);
    else entry.reject(new Error(event.data.error));
  };
  worker.onerror = (event) => {
    for (const entry of pending.values()) entry.reject(new Error(event.message || 'worker failed'));
    pending.clear();
  };
  return worker;
}

export function callWorker<K extends WorkerRequest['op']>(
  request: Extract<WorkerRequest, { op: K }>,
): Promise<ResultOf[K]> {
  const id = ++counter;
  return new Promise<ResultOf[K]>((resolve, reject) => {
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    ensureWorker().postMessage({ id, ...request });
  });
}
