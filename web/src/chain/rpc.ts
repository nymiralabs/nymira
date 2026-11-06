// A small JSON-RPC client for public, keyless NEAR RPC endpoints with fallback.
//
// What it will send is fixed by the callers in this folder: views at the final block, the
// account and access-key views of the vault, `storage_balance_of` / `ft_balance_of` for the payout
// account, and the status of the user's own deposit transaction. It never calls `note_status` and
// never queries a nullifier (a test asserts that on the wire).

export class RpcError extends Error {
  constructor(
    message: string,
    readonly code: string | number | undefined,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = 'RpcError';
  }
}

/** Every endpoint failed at the network level (timeout, 5xx, 429 without CORS, garbage). */
export class NetworkError extends Error {
  constructor(
    message: string,
    readonly causes: string[] = [],
  ) {
    super(message);
    this.name = 'NetworkError';
  }
}

export interface RpcOptions {
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
  /** After a network failure an endpoint is skipped for this long. */
  cooldownMs?: number;
  now?: () => number;
}

export class RpcClient {
  private readonly cooldown = new Map<string, number>();
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;
  private readonly cooldownMs: number;
  private readonly now: () => number;

  constructor(
    readonly urls: string[],
    options: RpcOptions = {},
  ) {
    if (urls.length === 0) throw new Error('no RPC endpoint configured');
    this.fetchImpl = options.fetchImpl ?? ((...a) => fetch(...a));
    this.timeoutMs = options.timeoutMs ?? 8000;
    this.cooldownMs = options.cooldownMs ?? 10_000;
    this.now = options.now ?? Date.now;
  }

  /** Raw JSON-RPC. A JSON-RPC error is thrown as RpcError and not retried elsewhere. */
  async call<T = unknown>(method: string, params: unknown): Promise<T> {
    const body = JSON.stringify({ jsonrpc: '2.0', id: 'nymira', method, params });
    const failures: string[] = [];
    const t = this.now();
    // Endpoints in a cooldown go last, so a recovering one is still tried if all others fail.
    const ordered = [...this.urls].sort(
      (a, b) => Number((this.cooldown.get(a) ?? 0) > t) - Number((this.cooldown.get(b) ?? 0) > t),
    );
    for (const url of ordered) {
      try {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), this.timeoutMs);
        let response: Response;
        try {
          response = await this.fetchImpl(url, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body,
            signal: controller.signal,
            credentials: 'omit',
            referrerPolicy: 'no-referrer',
          });
        } finally {
          clearTimeout(timer);
        }
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const parsed = (await response.json()) as {
          result?: T;
          error?: { code?: number; message?: string; name?: string; cause?: { name?: string } };
        };
        if (parsed.error) {
          const cause = parsed.error.cause?.name ?? parsed.error.name ?? parsed.error.code;
          throw new RpcError(parsed.error.message ?? 'rpc error', cause, parsed.error);
        }
        if (!('result' in parsed)) throw new Error('no result');
        return parsed.result as T;
      } catch (error) {
        if (error instanceof RpcError) throw error;
        this.cooldown.set(url, this.now() + this.cooldownMs);
        failures.push(`${new URL(url).host}: ${error instanceof Error ? error.message : error}`);
      }
    }
    throw new NetworkError('no RPC endpoint answered', failures);
  }

  /** The hash of the latest final block. */
  async finalBlockHash(): Promise<string> {
    const block = await this.call<{ header: { hash: string } }>('block', { finality: 'final' });
    return block.header.hash;
  }

  /**
   * The latest final block's time in Unix seconds. The vault judges every deadline (the exit
   * window, a proof's `valid_until`) by block time, so the app asks the chain, not the local clock.
   */
  async finalBlockTime(): Promise<number> {
    const block = await this.call<{ header: { timestamp?: number; timestamp_nanosec?: string } }>(
      'block',
      { finality: 'final' },
    );
    const ns = block.header.timestamp_nanosec ?? String(block.header.timestamp ?? '');
    if (!/^\d+$/.test(ns)) throw new Error('the block has no timestamp');
    return Number(BigInt(ns) / 1_000_000_000n);
  }

  /**
   * A view call at the final block (or at `blockHash`, so several calls read one state); the
   * contract's JSON result.
   */
  async view<T = unknown>(
    account: string,
    method: string,
    args: unknown = {},
    blockHash?: string,
  ): Promise<T> {
    const result = await this.call<{ result?: number[]; error?: string }>('query', {
      request_type: 'call_function',
      ...(blockHash ? { block_id: blockHash } : { finality: 'final' }),
      account_id: account,
      method_name: method,
      args_base64: btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify(args)))),
    });
    if (result.error) throw new RpcError(result.error, 'CONTRACT_PANIC');
    const bytes = new Uint8Array(result.result ?? []);
    const text = new TextDecoder().decode(bytes);
    return (text === '' ? null : JSON.parse(text)) as T;
  }

  viewAccount(account: string) {
    return this.call<{ amount: string; code_hash: string; storage_usage: number }>('query', {
      request_type: 'view_account',
      finality: 'final',
      account_id: account,
    });
  }

  accessKeyList(account: string) {
    return this.call<{ keys: { public_key: string; access_key: { permission: unknown } }[] }>(
      'query',
      { request_type: 'view_access_key_list', finality: 'final', account_id: account },
    );
  }

  /** The status of a transaction with its receipts (`EXPERIMENTAL_tx_status`). */
  txStatus(hash: string, senderAccount: string) {
    return this.call<TxStatus>('EXPERIMENTAL_tx_status', {
      tx_hash: hash,
      sender_account_id: senderAccount,
      wait_until: 'FINAL',
    });
  }
}

/** The parts of a FinalExecutionOutcome the app reads. */
export interface TxStatus {
  status?: unknown;
  transaction?: { hash?: string; signer_id?: string; receiver_id?: string };
  transaction_outcome?: { id?: string; outcome?: OutcomeBody };
  receipts_outcome?: { id?: string; outcome?: OutcomeBody }[];
}

export interface OutcomeBody {
  logs?: string[];
  receipt_ids?: string[];
  executor_id?: string;
  status?: unknown;
}
