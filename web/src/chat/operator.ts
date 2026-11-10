// The operator's HTTP API as the web app uses it (operator spec: `/v1/status`, the lease routes,
// the chat proxy and the request recovery route). Errors carry the `code` of the operator's error
// table; the app branches on that, never on message text.
import { splitObject } from '../note/rawjson';

export const TOKEN_HEADER = 'X-Nymira-Lease-Token';
export const PUZZLE_HEADER = 'X-Nymira-Puzzle';

export class OperatorError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly retryAfterSecs?: number,
  ) {
    super(message);
    this.name = 'OperatorError';
  }
}

/** The operator could not be reached at all (network, CORS, a refused origin). */
export class OperatorUnreachable extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'OperatorUnreachable';
  }
}

export interface IssuedLease {
  leaseId: string;
  token: string;
  clientRequestId: string;
  bound: string;
  issuedAt: number;
  expiresAt: number;
}

export interface Recovery {
  status: string;
  /** `RequestResponseV2` as the operator sent it, once it is finalized. */
  responseJson?: string;
}

type Json = Record<string, unknown>;

export class OperatorClient {
  private readonly base: string;
  constructor(
    base: string,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {
    this.base = base.replace(/\/+$/, '');
  }

  private async send(path: string, init: RequestInit = {}): Promise<Response> {
    try {
      return await this.fetchImpl(`${this.base}${path}`, {
        ...init,
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
      });
    } catch (e) {
      throw new OperatorUnreachable(e instanceof Error ? e.message : String(e));
    }
  }

  private static async fail(response: Response): Promise<never> {
    const body = (await response.json().catch(() => ({}))) as { error?: Json };
    const e = body.error ?? {};
    const header = Number(response.headers.get('retry-after'));
    throw new OperatorError(
      response.status,
      typeof e.code === 'string' ? e.code : '',
      typeof e.message === 'string' ? e.message : `HTTP ${response.status}`,
      typeof e.retry_after_secs === 'number'
        ? e.retry_after_secs
        : Number.isFinite(header) && header > 0
          ? header
          : undefined,
    );
  }

  async status(): Promise<Json> {
    const r = await this.send('/v1/status');
    if (!r.ok) return OperatorClient.fail(r);
    return (await r.json()) as Json;
  }

  /** `POST /v2/openrouter/leases`. `requestJson` is the wallet core's own text. */
  async openLease(requestJson: string, puzzle?: string): Promise<IssuedLease> {
    const r = await this.send('/v2/openrouter/leases', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        ...(puzzle ? { [PUZZLE_HEADER]: puzzle } : {}),
      },
      body: requestJson,
    });
    if (!r.ok) return OperatorClient.fail(r);
    const b = (await r.json()) as Json;
    const text = (k: string) => (typeof b[k] === 'string' ? (b[k] as string) : undefined);
    const num = (k: string) => (typeof b[k] === 'number' ? (b[k] as number) : undefined);
    const leaseId = text('lease_id');
    const token = text('lease_token');
    const bound = text('bound');
    const issuedAt = num('issued_at');
    const expiresAt = num('expires_at');
    if (!leaseId || !token || !bound || issuedAt === undefined || expiresAt === undefined) {
      throw new OperatorUnreachable('the operator’s lease answer is incomplete');
    }
    return {
      leaseId,
      token,
      clientRequestId: text('client_request_id') ?? '',
      bound,
      issuedAt,
      expiresAt,
    };
  }

  /** `POST /v2/openrouter/leases/{id}`: ends the lease so the operator settles it. */
  async retire(leaseId: string, token: string, keepalive = false): Promise<void> {
    const r = await this.send(`/v2/openrouter/leases/${encodeURIComponent(leaseId)}`, {
      method: 'POST',
      headers: { [TOKEN_HEADER]: token },
      keepalive,
    });
    if (!r.ok) return OperatorClient.fail(r);
  }

  /** `GET /v2/openrouter/leases/{id}`: the lease and, once ended, the provider's signatures. */
  async leaseStatus(leaseId: string, token: string): Promise<Json> {
    const r = await this.send(`/v2/openrouter/leases/${encodeURIComponent(leaseId)}`, {
      headers: { [TOKEN_HEADER]: token },
    });
    if (!r.ok) return OperatorClient.fail(r);
    return (await r.json()) as Json;
  }

  /** `GET /v2/requests/{client_request_id}`: the signed next state once it exists. */
  async recover(clientRequestId: string): Promise<Recovery> {
    const r = await this.send(`/v2/requests/${encodeURIComponent(clientRequestId)}`);
    if (r.status === 404) return { status: 'unknown' };
    if (!r.ok) return OperatorClient.fail(r);
    const parts = splitObject(await r.text());
    const status = parts.status ? (JSON.parse(parts.status) as string) : 'unknown';
    const response = parts.request_response;
    return { status, responseJson: response && response !== 'null' ? response : undefined };
  }

  /**
   * `GET /v2/nullifiers/{n}`: what the operator's records say about a request nullifier
   * (`reserved`, `finalized`, `clearance_reserved` or `unknown`). The operator already holds the
   * nullifier; no public node is asked.
   */
  async nullifierStatus(nullifier: string): Promise<string> {
    const r = await this.send(`/v2/nullifiers/${encodeURIComponent(nullifier)}`);
    if (!r.ok) return OperatorClient.fail(r);
    const body = (await r.json()) as Json;
    return typeof body.nullifier_status === 'string' ? body.nullifier_status : 'unknown';
  }

  /**
   * Sends a lease request again, exactly as kept, for the privacy panel's reuse check. It carries
   * no puzzle. An operator that accepts it has opened a lease, which the answer reports.
   */
  async resend(requestJson: string): Promise<{ accepted: true } | OperatorError> {
    const r = await this.send('/v2/openrouter/leases', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: requestJson,
    });
    if (r.ok) return { accepted: true };
    return OperatorClient.fail(r).catch((e: unknown) => {
      if (e instanceof OperatorError) return e;
      throw e;
    });
  }

  /**
   * `POST /v2/withdraw/clearance`: the ownership proof for a nullifier with no lease open. Returns
   * the operator's `ClearanceResponseV2` as JSON text, which the wallet core checks against the
   * pinned clearance key.
   */
  async clearance(bodyJson: string): Promise<string> {
    const r = await this.send('/v2/withdraw/clearance', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: bodyJson,
    });
    if (!r.ok) return OperatorClient.fail(r);
    return r.text();
  }

  /** `POST /v1/close`: the operator sends `mutual_close` and takes the close fee. Returns the hash. */
  async close(proof: string, inputs: string[]): Promise<string> {
    const r = await this.send('/v1/close', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ proof, inputs }),
    });
    if (!r.ok) return OperatorClient.fail(r);
    const body = (await r.json()) as { tx_hash?: unknown };
    return typeof body.tx_hash === 'string' ? body.tx_hash : '';
  }

  /** `POST /v1/chat/completions` with an already sealed body. The caller reads the bytes. */
  chat(body: string, leaseId: string, token: string, clientPub: string, modelPub: string) {
    return this.send('/v1/chat/completions', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'x-nymira-lease-id': leaseId,
        [TOKEN_HEADER]: token,
        'x-signing-algo': 'ed25519',
        'x-client-pub-key': clientPub,
        'x-model-pub-key': modelPub,
        'x-encryption-version': '2',
      },
      body,
    });
  }

  /** The error of a failed chat response as an `OperatorError`. */
  static failure(response: Response): Promise<never> {
    return OperatorClient.fail(response);
  }
}

/** What the user is told for each operator code. Capacity and provider limits differ on purpose. */
export function describeOperatorError(e: OperatorError | OperatorUnreachable): string {
  if (e instanceof OperatorUnreachable) {
    return `The operator could not be reached (${e.message}). Your note was not touched.`;
  }
  const wait = e.retryAfterSecs ? ` Try again in about ${e.retryAfterSecs} s.` : '';
  switch (e.code) {
    case 'operator_capacity':
      return `The operator is at capacity right now (its credit, slots or daily budget).${wait} This is the operator, not the model provider.`;
    case 'provider_rate_limit':
      return `The model provider is rate limiting requests.${wait} This is NEAR AI’s limit, not the operator’s capacity.`;
    case 'puzzle_required':
      return 'The operator asked for a proof of work and the answer was not accepted.';
    case 'root_stale':
      return 'The note tree changed while the proof was made.';
    case 'nullifier_in_exit':
      return 'This note is in an exit. It cannot be closed with the operator; finalize the exit after its deadline.';
    case 'nullifier_seen':
    case 'clearance_signed':
      return 'This note state was already used by another request or is being closed. Reload the note.';
    case 'nullifier_rate_limited':
      return `Too many attempts for this note state.${wait}`;
    case 'bound_class_invalid':
      return 'The operator does not offer this lease size.';
    case 'proof_expired':
      return 'The proof was too old when it arrived. Check that this device’s clock is right.';
    case 'proof_invalid':
      return 'The operator did not accept the proof.';
    case 'lease_token_invalid':
      return 'The operator did not accept this lease’s token.';
    case 'lease_not_found':
    case 'lease_ended':
      return 'This lease has ended.';
    case 'lease_concurrency':
      return 'Two requests are already running on this lease. Wait for one to finish.';
    case 'max_tokens_too_large':
      return 'The answer limit does not fit what is left of the lease.';
    case 'unsupported_request':
    case 'malformed_request':
    case 'unsupported_version':
      return `The operator refused the request (${e.code}): ${e.message}`;
    case 'recovery_mode':
    case 'replica_unreachable':
      return `The operator is not signing new leases right now (${e.code}).${wait}`;
    case 'emergency':
      return 'The operator stopped new leases after a key problem. Request an exit from the Close screen.';
    case 'lease_open':
      return 'A lease is still open on this note at the operator. Wait for it to settle, then try again.';
    case 'note_not_active':
      return 'The vault does not show this note as open (it may be closed or already in an exit).';
    case 'close_rate_limited':
      return `The operator’s close service is limited.${wait}`;
    case 'provider_error':
      return 'The model provider returned an error.';
    case 'provider_timeout':
      return 'The model provider did not answer in time.';
    case 'body_too_large':
      return 'The message is too large for one request.';
    default:
      return `The operator answered ${e.status}${e.code ? ` ${e.code}` : ''}: ${e.message}`;
  }
}
