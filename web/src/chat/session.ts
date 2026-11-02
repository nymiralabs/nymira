// One chat conversation on one note: attest the model, open a lease, send end-to-end encrypted
// requests, and on leaving retire the lease, install the signed successor state, check the
// signatures and the charge, and hand the finished record to the caller to back up.
//
// The order that matters (web spec stories 25 to 37):
//   vault pins ok -> model attested (no key is chosen before this) -> status nonce verified against
//   the state key -> tree read at one final block -> proof made in the worker -> the sealed state
//   with the bound is stored -> only then the request leaves.
import { checkVault } from '../chain/vault';
import type { RpcClient } from '../chain/rpc';
import { readSnapshot } from '../chain/tree';
import type { Profile } from '../profiles/types';
import type { LeaseConfigInput } from '../worker/protocol';
import { callWorker } from '../worker/client';
import { noteAad, type NoteRecord } from '../store/types';
import { putNote } from '../store/db';
import type { ActiveLease, LeaseRecord, NoteView } from '../note/lease';
import { ClientKeys, E2eeError, openResponse, openSseLine, sealRequest } from './e2ee';
import { fromHex, toHex } from '../crypto/bytes';
import {
  BOUND_SHARE_PCT,
  charge as chargeOf,
  costNano,
  maxTokens,
  type ChargeParams,
} from './charge';
import { GateError, checkParams, puzzleDifficulty, requestTime, verifyStatus, type SignatureCheck } from './gate';
import { DEFAULT_MODEL, chooseBound, modelById } from './models';
import {
  OperatorClient,
  OperatorError,
  OperatorUnreachable,
  describeOperatorError,
} from './operator';
import type { Policy } from './attest/policy';
import { AttestationError, verifyReports, type Attested, type ModelReport, type QuoteVerifier } from './attest/verify';
import { badgeLabel, verifyCompletionSignature, weakest, type Trust } from './attest/signature';

export interface Message {
  role: 'system' | 'user' | 'assistant';
  content: string;
}

export interface Deps {
  profile: Profile;
  rpc: RpcClient;
  operator: OperatorClient;
  fetchImpl?: typeof fetch;
  policy: Policy;
  quote: QuoteVerifier;
  signatureCheck: SignatureCheck;
  getKey: () => CryptoKey;
  /** Milliseconds. */
  now?: () => number;
  random?: () => string;
}

export type Failure = { kind: string; message: string; retryable?: boolean };

export class ChatError extends Error {
  constructor(
    readonly kind: string,
    message: string,
  ) {
    super(message);
    this.name = 'ChatError';
  }
}

export interface Answer {
  id: number;
  text: string;
  reasoning: string;
  promptTokens: number;
  completionTokens: number;
  /** Micro-USDC the usage costs at the pinned prices (not the lease charge). */
  usageMicro: number;
  streamed: boolean;
  chatId?: string;
  /** Set when the lease ends and the provider's signature has been checked. */
  trust?: Trust;
}

interface Exchange {
  answer: Answer;
  model: string;
  request: Uint8Array;
  response: Uint8Array;
}

export interface Settled {
  record: LeaseRecord;
  trust: Trust;
  charge: bigint;
  expected: bigint;
  overcharged: boolean;
  balance: string;
}

const ATTEST_TTL_MS = 300_000;
const utf8 = (s: string) => new TextEncoder().encode(s);

export function estimateInputTokens(messages: Message[]): bigint {
  return messages.reduce((n, m) => n + BigInt(utf8(m.content).length), 0n);
}

/** Reads `config()`'s lease parameters as BigInt. */
export function chargeParams(profile: Profile): ChargeParams {
  const c = profile.pins?.config;
  if (!c) throw new ChatError('no_pins', 'This build has no vault pins.');
  return { step: BigInt(c.charge_step), minCharge: BigInt(c.min_charge), slotRate: BigInt(c.slot_rate) };
}

export class ChatSession {
  readonly keys = new ClientKeys();
  private readonly http: typeof fetch;
  private readonly now: () => number;
  private attested = new Map<string, { at: number; value: Attested }>();
  private exchanges: Exchange[] = [];
  private counter = 0;
  private spentMicro = 0n;
  /** The current record of the note (its sealed payload moves on after every step). */
  private note: NoteRecord;
  lease: ActiveLease | null = null;
  attestation: Attested | null = null;

  constructor(
    private readonly deps: Deps,
    note: NoteRecord,
    private readonly longJob = false,
  ) {
    this.note = note;
    this.http = deps.fetchImpl ?? ((...a) => fetch(...a));
    this.now = deps.now ?? Date.now;
  }

  get record(): NoteRecord {
    return this.note;
  }
  private get aad(): string {
    return noteAad(this.note.network, this.note.vault, this.note.id);
  }
  private nowSecs(): number {
    return Math.floor(this.now() / 1000);
  }

  private async store(sealed: NoteRecord['sealed']): Promise<void> {
    this.note = { ...this.note, sealed, updatedAt: this.now() };
    await putNote(this.note);
  }

  private config(): LeaseConfigInput {
    const pins = this.deps.profile.pins;
    if (!pins) throw new ChatError('no_pins', 'This build has no vault pins.');
    return {
      chainId: pins.config.chain_id,
      operatorPk: pins.config.operator_pk,
      clearancePk: pins.config.clearance_pk,
      vault: this.deps.profile.vaultAccount,
    };
  }

  async view(): Promise<NoteView> {
    return callWorker({ op: 'readNote', key: this.deps.getKey(), aad: this.aad, sealed: this.note.sealed });
  }

  // ---- attestation ----

  private async getJson(path: string, headers: Record<string, string> = {}): Promise<unknown> {
    const r = await this.http(`${this.deps.profile.gatewayUrl.replace(/\/+$/, '')}${path}`, {
      headers,
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
    });
    if (!r.ok) throw new ChatError('attestation_unreachable', `The attestation report could not be read (HTTP ${r.status}).`);
    return r.json();
  }

  /** Fetches and verifies the model's and the gateway's evidence, with fresh nonces. */
  async attest(model: string): Promise<Attested> {
    const cached = this.attested.get(model);
    if (cached && this.now() - cached.at < ATTEST_TTL_MS) return cached.value;
    const nonce = () => toHex(crypto.getRandomValues(new Uint8Array(32)));
    const [modelNonce, gatewayNonce] = [nonce(), nonce()];
    try {
      const q = new URLSearchParams({
        model,
        provider: 'near',
        nonce: modelNonce,
        include_tls_fingerprint: 'false',
        signing_algo: 'ed25519',
      });
      const modelReport = (await this.getJson(`/v1/attestation/report?${q}`, {
        'x-no-aliasing': 'true',
      })) as ModelReport;
      const g = new URLSearchParams({
        nonce: gatewayNonce,
        signing_algo: 'ed25519',
        include_tls_fingerprint: 'false',
      });
      const gatewayReport = (await this.getJson(`/v1/attestation/report?${g}`)) as ModelReport;
      const value = await verifyReports(
        model,
        modelReport,
        modelNonce,
        gatewayReport,
        gatewayNonce,
        this.deps.policy,
        this.deps.quote,
      );
      this.attested.set(model, { at: this.now(), value });
      this.attestation = value;
      return value;
    } catch (e) {
      if (e instanceof AttestationError) {
        throw new ChatError('attestation_failed', `The model’s attestation was refused (${e.code}): ${e.message}. Nothing was encrypted and no lease was opened.`);
      }
      throw e;
    }
  }

  // ---- lease ----

  private async checkedStatus() {
    const profile = this.deps.profile;
    let status: Record<string, unknown>;
    try {
      status = await this.deps.operator.status();
    } catch (e) {
      throw this.wrap(e);
    }
    try {
      const nonce = verifyStatus(status, profile.pins!.config.operator_pk, this.nowSecs(), this.deps.signatureCheck);
      checkParams(status, profile.pins!.config);
      return { status, nonce };
    } catch (e) {
      if (e instanceof GateError) throw new ChatError(`gate_${e.refusal.kind}`, e.message);
      throw e;
    }
  }

  private wrap(e: unknown): Error {
    if (e instanceof OperatorError || e instanceof OperatorUnreachable) {
      return new ChatError(e instanceof OperatorError ? e.code || 'operator_error' : 'unreachable', describeOperatorError(e));
    }
    return e instanceof Error ? e : new Error(String(e));
  }

  /** Opens a lease on this note. Throws `ChatError` with a user-facing message. */
  async openLease(): Promise<void> {
    if (this.lease?.token) return;
    const profile = this.deps.profile;
    const pins = profile.pins;
    if (!pins) throw new ChatError('no_pins', 'This build has no vault pins, so chat is off.');
    if (!profile.provingKey) throw new ChatError('no_proving_key', 'The proving key for this deployment is not published yet, so chat is off.');
    const vault = await checkVault(this.deps.rpc, profile.vaultAccount, pins, this.now);
    if (vault.status !== 'ok') {
      throw new ChatError('vault_changed', vault.status === 'mismatch' ? `The vault no longer matches this build (${vault.diffs.join(', ')}). No new lease.` : 'The vault could not be checked. No new lease.');
    }
    const { status: first } = await this.checkedStatus();
    if (first.emergency_notice) {
      throw new ChatError('emergency', `The operator posted an emergency notice: ${String(first.emergency_notice)}. No new lease; close your note or request an exit.`);
    }
    const bound = chooseBound(pins.config.bound_classes.map(BigInt), this.longJob);
    if (bound === null) throw new ChatError('bound_class_missing', 'The vault lists no lease class of that size.');
    const view = await this.view();
    if (view.pending) throw new ChatError('lease_pending', 'An earlier lease on this note is not settled yet. Settle it first.');

    const build = async () => {
      const { status, nonce } = await this.checkedStatus();
      const tree = await readSnapshot(this.deps.rpc, profile.vaultAccount);
      const leaf = tree.leaves[view.noteId];
      if (leaf === undefined || /^0x0+$/.test(leaf)) {
        throw new ChatError('leaf_missing', 'This note’s leaf is not in the tree yet (wait for the operator’s next root update) or was retired.');
      }
      const prepared = await callWorker({
        op: 'prepareLease',
        key: this.deps.getKey(),
        aad: this.aad,
        sealed: this.note.sealed,
        input: {
          config: this.config(),
          noteId: view.noteId,
          bound: bound.toString(),
          tree,
          requestTime: requestTime(nonce),
          clientRequestId: this.deps.random?.() ?? crypto.randomUUID(),
          createdAtMs: this.now(),
          openedAt: this.nowSecs(),
        },
      });
      // The bound for this state is stored before the proof leaves this page.
      await this.store(prepared.sealed);
      return { prepared, status, nonce };
    };

    let { prepared } = await build();
    let rebuilt = false;
    let puzzled = false;
    for (;;) {
      const { status, nonce } = await this.checkedStatus();
      const difficulty = puzzleDifficulty(status);
      const puzzle = difficulty > 0 ? await callWorker({ op: 'solvePuzzle', nonce: nonce.nonce, difficulty }) : undefined;
      try {
        const issued = await this.deps.operator.openLease(prepared.requestJson, puzzle);
        const sealed = await callWorker({
          op: 'leaseOpened',
          key: this.deps.getKey(),
          aad: this.aad,
          sealed: this.note.sealed,
          lease: { leaseId: issued.leaseId, token: issued.token, issuedAt: issued.issuedAt, expiresAt: issued.expiresAt },
        });
        await this.store(sealed);
        this.lease = (await this.view()).active ?? null;
        this.spentMicro = 0n;
        return;
      } catch (e) {
        if (e instanceof OperatorError && e.code === 'puzzle_required' && !puzzled) {
          puzzled = true;
          continue;
        }
        if (e instanceof OperatorError && e.code === 'root_stale' && !rebuilt) {
          rebuilt = true;
          // The operator refused the proof before binding its nullifier: set it aside, rebuild the
          // tree, prove again from the same state.
          await this.store(
            await callWorker({ op: 'discardPending', key: this.deps.getKey(), aad: this.aad, sealed: this.note.sealed }),
          );
          ({ prepared } = await build());
          continue;
        }
        if (e instanceof OperatorError && e.status < 500 && e.code !== 'operator_capacity') {
          // A refusal before the lease exists: nothing was spent; the pending proof is set aside.
          if (e.code === 'root_stale' || e.code === 'proof_expired' || e.code === 'bound_class_invalid' || e.code === 'proof_invalid' || e.code === 'malformed_request') {
            await this.store(
              await callWorker({ op: 'discardPending', key: this.deps.getKey(), aad: this.aad, sealed: this.note.sealed }),
            );
          }
        }
        if (e instanceof OperatorError && e.code === 'operator_capacity') {
          await this.store(
            await callWorker({ op: 'discardPending', key: this.deps.getKey(), aad: this.aad, sealed: this.note.sealed }),
          );
        }
        throw this.wrap(e);
      }
    }
  }

  // ---- chat ----

  /**
   * Sends one conversation turn. `messages` is the whole conversation so far (the model has no
   * memory of its own). Returns the opened answer; the badge comes when the lease ends.
   */
  async send(
    messages: Message[],
    opts: { model?: string; stream?: boolean; temperature?: number; onDelta?: (text: string) => void } = {},
  ): Promise<Answer> {
    const model = opts.model ?? DEFAULT_MODEL;
    const spec = modelById(model);
    if (!spec) throw new ChatError('unknown_model', `The model ${model} is not on the pinned list.`);
    const attested = await this.attest(model);
    await this.openLease();
    const lease = this.lease;
    if (!lease?.token) throw new ChatError('no_lease', 'No lease is open.');
    const bound = BigInt(lease.record.boundClass);
    const remaining = bound - this.spentMicro;
    const input = estimateInputTokens(messages);
    const max = maxTokens(remaining, BOUND_SHARE_PCT, spec, input);
    if (max === null) {
      throw new ChatError('no_funds', 'This lease cannot pay for this prompt and an answer. Start a new conversation or shorten it.');
    }
    const stream = opts.stream === true;
    const modelPub = fromHex(attested.model.signingKey);
    let sealed: Record<string, unknown>;
    try {
      sealed = sealRequest(
        {
          model,
          messages,
          stream,
          ...(opts.temperature !== undefined ? { temperature: opts.temperature } : {}),
        },
        modelPub,
        max,
      );
    } catch (e) {
      if (e instanceof E2eeError) throw new ChatError('cannot_encrypt', e.message);
      throw e;
    }
    const body = JSON.stringify(sealed);
    let response: Response;
    try {
      response = await this.deps.operator.chat(body, lease.record.leaseId, lease.token, this.keys.publicHex, attested.model.signingKey);
    } catch (e) {
      throw this.wrap(e);
    }
    if (!response.ok) {
      try {
        await OperatorClient.failure(response);
      } catch (e) {
        throw this.wrap(e);
      }
    }
    const id = ++this.counter;
    let text = '';
    let reasoning = '';
    let usage: { prompt_tokens?: number; completion_tokens?: number } | undefined;
    let chatId: string | undefined;
    let rawBytes: Uint8Array;
    try {
      if (stream) {
        const chunks: Uint8Array[] = [];
        const reader = response.body!.getReader();
        const decoder = new TextDecoder();
        let pending = '';
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          chunks.push(value);
          pending += decoder.decode(value, { stream: true });
          let nl: number;
          while ((nl = pending.indexOf('\n')) >= 0) {
            const line = pending.slice(0, nl).replace(/\r$/, '');
            pending = pending.slice(nl + 1);
            const opened = openSseLine(this.keys, line);
            if (opened.startsWith('data:') && !opened.includes('[DONE]')) {
              const chunk = JSON.parse(opened.slice(5)) as {
                id?: string;
                usage?: typeof usage;
                choices?: { delta?: { content?: string; reasoning_content?: string } }[];
              };
              chatId ??= chunk.id;
              if (chunk.usage) usage = chunk.usage;
              const delta = chunk.choices?.[0]?.delta;
              if (delta?.content) {
                text += delta.content;
                opts.onDelta?.(text);
              }
              if (delta?.reasoning_content) reasoning += delta.reasoning_content;
            }
          }
        }
        rawBytes = concat(chunks);
      } else {
        rawBytes = new Uint8Array(await response.arrayBuffer());
        const answer = JSON.parse(new TextDecoder().decode(rawBytes)) as {
          id?: string;
          usage?: typeof usage;
          choices?: { message?: { content?: string | null; reasoning_content?: string | null } }[];
        };
        openResponse(this.keys, answer as unknown as Record<string, unknown>);
        chatId = answer.id;
        usage = answer.usage;
        text = answer.choices?.[0]?.message?.content ?? '';
        reasoning = answer.choices?.[0]?.message?.reasoning_content ?? '';
      }
    } catch (e) {
      if (e instanceof E2eeError) throw new ChatError('decrypt_failed', e.message);
      throw new ChatError('provider_error', 'The answer could not be read.');
    }
    if (!usage) throw new ChatError('provider_error', 'The answer carries no usage, so its cost cannot be checked.');
    const prompt = usage.prompt_tokens ?? 0;
    const completion = usage.completion_tokens ?? 0;
    const cost = costNano(spec, BigInt(prompt), BigInt(completion));
    const usageMicro = Number((cost + 999n) / 1000n);
    this.spentMicro += BigInt(usageMicro);
    const answer: Answer = { id, text, reasoning, promptTokens: prompt, completionTokens: completion, usageMicro, streamed: stream, chatId };
    this.exchanges.push({ answer, model, request: utf8(body), response: rawBytes });
    await this.store(
      await callWorker({
        op: 'recordExchange',
        key: this.deps.getKey(),
        aad: this.aad,
        sealed: this.note.sealed,
        use: {
          model,
          sampling: { stream, ...(opts.temperature !== undefined ? { temperature: opts.temperature } : {}) },
          messageCount: messages.length,
          messageBytes: messages.reduce((n, m) => n + utf8(m.content).length, 0),
          promptTokens: prompt,
          completionTokens: completion,
        },
      }),
    );
    return answer;
  }

  // ---- leaving ----

  /**
   * Retires the lease, waits for the operator to settle, checks signatures and charge, installs the
   * signed successor and returns the finished record. Leaving a conversation calls this.
   */
  async end(opts: { pollMs?: number; maxWaitMs?: number } = {}): Promise<Settled> {
    const lease = this.lease;
    if (!lease?.token) throw new ChatError('no_lease', 'No lease is open.');
    const { operator } = this.deps;
    try {
      await operator.retire(lease.record.leaseId, lease.token);
    } catch (e) {
      // Already retired is fine: settlement is what matters.
      if (!(e instanceof OperatorError && (e.code === 'lease_ended' || e.status === 410))) throw this.wrap(e);
    }
    const poll = opts.pollMs ?? 1500;
    const deadline = this.now() + (opts.maxWaitMs ?? 120_000);
    let responseJson: string | undefined;
    while (this.now() < deadline) {
      try {
        responseJson = (await operator.recover(lease.clientRequestId)).responseJson;
      } catch (e) {
        throw this.wrap(e);
      }
      if (responseJson) break;
      await new Promise((r) => setTimeout(r, poll));
    }
    if (!responseJson) {
      throw new ChatError('settling', 'The operator has not settled this lease yet. Your note keeps the pending request; open the note again later to finish.');
    }
    return this.settle(responseJson);
  }

  private async settle(responseJson: string): Promise<Settled> {
    const lease = this.lease!;
    const { operator, profile } = this.deps;
    const signatures = await operator
      .leaseStatus(lease.record.leaseId, lease.token!)
      .then((s) => ((s.provider_evidence as { signatures?: { chat_id?: string; signature?: unknown }[] } | undefined)?.signatures ?? []))
      .catch(() => []);
    const trusts: Trust[] = [];
    for (const x of this.exchanges) {
      const found = signatures.find((s) => s.chat_id === x.answer.chatId);
      const attested = this.attested.get(x.model)?.value;
      const trust: Trust = !attested
        ? { result: 'not_verified', reason: 'no verified attestation for this model' }
        : !x.answer.chatId
          ? { result: 'not_verified', reason: 'the answer has no chat id' }
          : verifyCompletionSignature(attested, found?.signature as never, x.model, x.request, x.response);
      x.answer.trust = trust;
      trusts.push(trust);
    }
    const trust = weakest(trusts);
    const warnings: string[] = [];
    if (trust.result === 'not_verified') warnings.push(`not verified: ${trust.reason}`);

    // The charge, recomputed with the operator's formula from the usage the answers reported.
    const params = chargeParams(profile);
    const bound = BigInt(lease.record.boundClass);
    const cost = this.exchanges.reduce((n, x) => {
      const spec = modelById(x.model)!;
      return n + costNano(spec, BigInt(x.answer.promptTokens), BigInt(x.answer.completionTokens));
    }, 0n);
    // The local clock, not a figure the operator reports; the status nonce was checked against it.
    const held = BigInt(Math.max(0, this.nowSecs() - (lease.issuedAt ?? lease.record.openedAt)));
    const expected = chargeOf(bound, cost, held, params);
    const reported = BigInt(String((JSON.parse(responseJson) as { charge_applied: number | string }).charge_applied));
    const overcharged = reported > expected + params.step;
    if (overcharged) {
      warnings.push(`the operator charged ${reported} micro-USDC; the signed usage and the vault’s rules give ${expected} (more than one charge step of ${params.step} above)`);
    }
    let completed;
    try {
      completed = await callWorker({
        op: 'completeLease',
        key: this.deps.getKey(),
        aad: this.aad,
        sealed: this.note.sealed,
        config: this.config(),
        settlement: {
          responseJson,
          closedAt: this.nowSecs(),
          expectedCharge: expected.toString(),
          trust: trust.result,
          warnings,
        },
      });
    } catch (e) {
      throw new ChatError('invalid_successor', `The operator’s signed state failed the wallet checks (${e instanceof Error ? e.message : String(e)}). Your note keeps its pending request; do not open another lease and consider an exit.`);
    }
    await this.store(completed.sealed);
    this.lease = null;
    const view = await this.view();
    return { record: completed.record, trust, charge: reported, expected, overcharged, balance: view.balance };
  }

  /** The answers so far, with their badges once the lease has ended. */
  get answers(): Answer[] {
    return this.exchanges.map((x) => x.answer);
  }
}

function concat(chunks: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

export { badgeLabel };
