// A mock operator and NEAR AI gateway for the chat tests, running in the Playwright worker.
//
// What is real: the status nonce is signed with a real zkapi state key (test operator seed 1); a
// lease request's Groth16 proof is verified with the development verifying key; the successor
// state is computed and signed by the zkapi operator algebra, so the app's wallet core runs S1 to
// S6 on a genuine response; chat bodies are decrypted with an Ed25519 model key and answered
// encrypted to the client's key; completion signatures are real Ed25519 over the SHA-256 of the
// exact bytes; the report's event log replays to its RTMR3.
// What is not: the Intel quote (a test quote the mock build accepts) and the NVIDIA payload.
// The unit tests cover the real quote on recorded NEAR AI evidence.
import { readFileSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { ed25519 } from '@noble/curves/ed25519.js';
import { sha256, sha384 } from '@noble/hashes/sha2.js';
import { charge as chargeOf, costNano, type ChargeParams } from '../src/chat/charge';
import { ClientKeys, decrypt, encrypt } from '../src/chat/e2ee';
import { modelById } from '../src/chat/models';
import { fromHex, toHex } from '../src/crypto/bytes';
import { leadingZeroBits } from '../src/chat/puzzle';
import { splitObject } from '../src/note/rawjson';
import {
  initSync as initZk,
  browser_verify_request,
} from '../src/wasm/zkapi-browser-dev/zkapi_browser.js';
import * as op from '../src/wasm/web-checks-test/nymira_web_wasm.js';
import {
  contractAddress,
  initSync as initCodec,
} from '../src/wasm/near-codec/nymira_near_codec_wasm.js';

export const OPERATOR_PORT = 18546;
export const GATEWAY_PORT = 18548;
export const APP_ORIGIN = 'http://localhost:4173';

const root = new URL('../', import.meta.url);
const policy = JSON.parse(readFileSync(new URL('pins/attestation-policy.json', root), 'utf8')) as {
  deployments: {
    role: string;
    compose_hashes: string[];
    os_image_hash: string;
    mr_td: string;
    rtmr0: string;
    rtmr1: string;
    rtmr2: string;
  }[];
};
const vault = JSON.parse(readFileSync(new URL('test-fixtures/mock-vault.json', root), 'utf8')) as {
  vaultAccount: string;
  pins: { config: Record<string, unknown> };
};
const requestVk = readFileSync(
  new URL('../vendor/zkapi/protocol/setup/v2-concession-dev/request.vk', root),
);
const withdrawalVk = readFileSync(
  new URL('../vendor/zkapi/protocol/setup/v2-concession-dev/withdrawal.vk', root),
);
const CHAIN_ID = BigInt(vault.pins.config.chain_id as string);

let ready = false;
function initWasm() {
  if (ready) return;
  initZk({
    module: readFileSync(new URL('src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm', root)),
  });
  op.initSync({
    module: readFileSync(new URL('src/wasm/web-checks-test/nymira_web_wasm_bg.wasm', root)),
  });
  initCodec({
    module: readFileSync(new URL('src/wasm/near-codec/nymira_near_codec_wasm_bg.wasm', root)),
  });
  ready = true;
}

export type SignatureMode = 'auto' | 'provider_tee' | 'gateway' | 'bad' | 'none';

export interface Scenario {
  puzzleDifficulty: number;
  /** Answer `root_stale` to this many lease requests before accepting. */
  rootStale: number;
  /** `operator`: chat answers 429 operator_capacity; `provider`: 429 provider_rate_limit. */
  chatLimit: '' | 'operator' | 'provider';
  leaseLimit: '' | 'operator';
  /** Micro-USDC added to every charge. */
  inflate: number;
  signature: SignatureMode;
  tcb: string;
  /** The model report names a signing key that is not the quote's signer. */
  unboundKey: boolean;
  /** The status nonce is signed with another key. */
  badStatusSignature: boolean;
  emergency: string;
  /** An error code the clearance route answers with instead of a signature (HTTP 409). */
  clearanceError: string;
  /** An error code `POST /v1/close` answers with (HTTP 429). */
  closeError: string;
  /** The reason `nullifier_seen` carries when a lease request is sent a second time. */
  refusal: string;
  /** Overrides of the lease parameters the status repeats. */
  params: Record<string, unknown>;
}

const fresh = (): Scenario => ({
  puzzleDifficulty: 0,
  rootStale: 0,
  chatLimit: '',
  leaseLimit: '',
  inflate: 0,
  signature: 'auto',
  tcb: 'UpToDate',
  unboundKey: false,
  badStatusSignature: false,
  emergency: '',
  clearanceError: '',
  closeError: '',
  refusal: 'this request nullifier is already used',
  params: {},
});

export interface Lease {
  id: string;
  token: string;
  clientRequestId: string;
  requestJson: string;
  /** `request_nullifier` of the lease request. */
  nullifier: string;
  bound: bigint;
  issuedAt: number;
  retired: boolean;
  chats: {
    chatId: string;
    request: Uint8Array;
    response: Uint8Array;
    model: string;
    prompt: number;
    completion: number;
    stream: boolean;
  }[];
}

/** What a close or exit call carries, read back the way the vault reads it. */
export interface DecodedCall {
  /** The proof verifies for these statement words under the development withdrawal key. */
  valid: boolean;
  /** 0 escape, 1 mutual close, 2 ownership. */
  kind: number;
  noteId: number;
  finalBalance: bigint;
  conceded: bigint;
  validUntil: number;
  /** The 20-byte destination, 40 hex digits. */
  destination: string;
  nullifier: string;
  nextNullifier: string;
}

export interface CloseRecord extends DecodedCall {
  args: { proof: string; inputs: string[] };
}

/** `{proof, inputs}` as `mutual_close`, `request_exit` and `POST /v1/close` take them. */
export function decodeCall(args: { proof: string; inputs: string[] }): DecodedCall {
  initWasm();
  const words = args.inputs.map((w) => Buffer.from(w, 'base64').toString('hex'));
  if (words.length !== 16 || words.some((w) => w.length !== 64))
    throw new Error('a call carries 16 words of 32 bytes');
  const big = (i: number) => BigInt(`0x${words[i]}`);
  return {
    valid: op.test_operator_verify_withdrawal_words(
      withdrawalVk,
      JSON.stringify(words),
      args.proof,
    ),
    kind: Number(big(12)),
    noteId: Number(big(8) >> 192n),
    finalBalance: big(9),
    conceded: big(14),
    validUntil: Number(big(15)),
    destination: words[10]!.slice(24),
    nullifier: `0x${words[11]}`,
    nextNullifier: `0x${words[13]}`,
  };
}

export interface Seen {
  method: string;
  path: string;
  headers: Record<string, string | string[] | undefined>;
  body?: string;
}

const MODEL_SEED = new Uint8Array(32).fill(0x31);
const GATEWAY_SEED = new Uint8Array(32).fill(0x32);
const modelKey = ed25519.getPublicKey(MODEL_SEED);
const gatewayKey = ed25519.getPublicKey(GATEWAY_SEED);

export class MockStack {
  scenario: Scenario = fresh();
  leases = new Map<string, Lease>();
  seen: Seen[] = [];
  /** `request_time` of every lease request, and the slot times the status signed. */
  requestTimes: number[] = [];
  statusTimes: number[] = [];
  /** Plaintext the mock operator could see on the wire: it must never include message text. */
  wire: string[] = [];
  /** Nullifiers the operator signed a clearance for, and the closes it took through /v1/close. */
  clearances: string[] = [];
  closes: CloseRecord[] = [];
  /** Called when the operator "sends" a close, so a test can change what the chain shows. */
  onClose?: (record: CloseRecord) => Promise<void>;
  private servers: Server[] = [];
  private nonces: string[] = [];
  private issuedCount = 0;

  reset() {
    this.scenario = fresh();
    this.leases.clear();
    this.seen = [];
    this.requestTimes = [];
    this.statusTimes = [];
    this.wire = [];
    this.clearances = [];
    this.closes = [];
    this.onClose = undefined;
    this.issuedCount = 0;
  }

  set(patch: Partial<Scenario>) {
    this.scenario = { ...this.scenario, ...patch };
  }

  get modelPublicKey() {
    return toHex(modelKey);
  }

  async start() {
    initWasm();
    this.servers = [
      createServer((q, s) => void this.operator(q, s)).listen(OPERATOR_PORT, '127.0.0.1'),
      createServer((q, s) => void this.gateway(q, s)).listen(GATEWAY_PORT, '127.0.0.1'),
    ];
    await Promise.all(this.servers.map((s) => new Promise((r) => s.once('listening', r))));
  }

  async stop() {
    await Promise.all(this.servers.map((s) => new Promise((r) => s.close(r))));
  }

  // ---- plumbing ----

  private cors(res: ServerResponse, req: IncomingMessage) {
    if (req.headers.origin === APP_ORIGIN) {
      res.setHeader('access-control-allow-origin', APP_ORIGIN);
      res.setHeader('vary', 'origin');
      res.setHeader(
        'access-control-allow-headers',
        req.headers['access-control-request-headers'] ?? '*',
      );
      res.setHeader('access-control-allow-methods', 'GET, POST, OPTIONS');
      res.setHeader('access-control-expose-headers', 'Retry-After');
    }
  }

  private async body(req: IncomingMessage): Promise<string> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    return Buffer.concat(chunks).toString('utf8');
  }

  private json(res: ServerResponse, status: number, value: unknown, raw?: string) {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(raw ?? JSON.stringify(value));
  }

  private error(res: ServerResponse, status: number, code: string, message = code, retry?: number) {
    this.json(res, status, {
      error: { code, message, ...(retry ? { retry_after_secs: retry } : {}) },
    });
  }

  // ---- the operator ----

  private slot() {
    const ts = Math.floor(Date.now() / 5000) * 5;
    const nonce = toHex(crypto.getRandomValues(new Uint8Array(16)));
    this.nonces = [nonce, ...this.nonces].slice(0, 6);
    return { nonce, host: 'operator.test', ts };
  }

  private status() {
    const s = this.scenario;
    const slot = this.slot();
    this.statusTimes.push(slot.ts);
    const seed = s.badStatusSignature ? 9n : 1n;
    const c = vault.pins.config;
    return {
      version: 1,
      operator_pk: c.operator_pk,
      clearance_pk: c.clearance_pk,
      vault: vault.vaultAccount,
      note_fee: c.note_fee,
      close_fee: c.close_fee,
      bound_classes: c.bound_classes,
      charge_step: c.charge_step,
      min_charge: c.min_charge,
      slot_rate: c.slot_rate,
      ...s.params,
      nonce: slot,
      nonce_sig: JSON.parse(
        op.test_operator_sign_status(seed, slot.nonce, slot.host, BigInt(slot.ts)),
      ),
      admission: { load_pct: 0, retry_after_secs: 0, puzzle_difficulty: s.puzzleDifficulty },
      emergency_notice: s.emergency || null,
      future_field: { ignored: true },
    };
  }

  private params(): ChargeParams {
    const c = vault.pins.config;
    return {
      step: BigInt(c.charge_step as string),
      minCharge: BigInt(c.min_charge as string),
      slotRate: BigInt(c.slot_rate as number),
    };
  }

  private authorize(
    req: IncomingMessage,
    res: ServerResponse,
    lease: Lease | undefined,
  ): lease is Lease {
    if (!lease) {
      this.error(res, 404, 'lease_not_found');
      return false;
    }
    if (req.headers['x-nymira-lease-token'] !== lease.token) {
      this.error(res, 401, 'lease_token_invalid');
      return false;
    }
    return true;
  }

  private async operator(req: IncomingMessage, res: ServerResponse) {
    this.cors(res, req);
    if (req.method === 'OPTIONS') return void res.writeHead(204).end();
    const url = new URL(req.url ?? '/', 'http://x');
    const body = req.method === 'POST' ? await this.body(req) : '';
    this.seen.push({
      method: req.method ?? '',
      path: url.pathname,
      headers: { ...req.headers },
      body:
        url.pathname === '/v1/chat/completions' ||
        url.pathname === '/v2/openrouter/leases' ||
        url.pathname === '/v2/withdraw/clearance' ||
        url.pathname === '/v1/close'
          ? body
          : undefined,
    });
    const s = this.scenario;
    try {
      if (req.method === 'GET' && url.pathname === '/v1/status')
        return this.json(res, 200, this.status());

      if (req.method === 'POST' && url.pathname === '/v2/openrouter/leases') {
        this.requestTimes.push(
          Number(JSON.parse(splitObject(splitObject(body).public_inputs!).request_time!)),
        );
        if (s.leaseLimit === 'operator')
          return this.error(res, 429, 'operator_capacity', 'credit', 30);
        if (s.puzzleDifficulty > 0) {
          const header = String(req.headers['x-nymira-puzzle'] ?? '');
          const [nonce, counter] = header.split(':');
          const ok =
            nonce !== undefined &&
            counter !== undefined &&
            this.nonces.includes(nonce) &&
            leadingZeroBits(sha256(new TextEncoder().encode(`${nonce}:${counter}`))) >=
              s.puzzleDifficulty;
          if (!ok) return this.error(res, 429, 'puzzle_required');
        }
        if (s.rootStale > 0) {
          s.rootStale -= 1;
          return this.error(res, 409, 'root_stale');
        }
        const parts = splitObject(body);
        // The real operator keeps the 64-digit form; the proof library drops leading zeros.
        const nullifier = (
          JSON.parse(splitObject(parts.public_inputs!).request_nullifier!) as string
        ).replace(/^0x([0-9a-f]+)$/i, (_, h: string) => `0x${h.padStart(64, '0')}`);
        // A nullifier the operator already holds is refused before anything else is checked.
        if ([...this.leases.values()].some((l) => l.nullifier === nullifier))
          return this.error(res, 409, 'nullifier_seen', s.refusal);
        const verified = browser_verify_request(
          parts.public_inputs!,
          (JSON.parse(parts.proof!) as { proof: string }).proof,
          requestVk,
        );
        if (!verified) return this.error(res, 422, 'proof_invalid');
        const inputs = splitObject(parts.public_inputs!);
        const bound = BigInt(JSON.parse(inputs.solvency_bound!) as number | string);
        const lease: Lease = {
          id: `lease-${++this.issuedCount}`,
          token: toHex(crypto.getRandomValues(new Uint8Array(16))),
          clientRequestId: JSON.parse(parts.client_request_id!) as string,
          requestJson: body,
          nullifier,
          bound,
          issuedAt: Math.floor(Date.now() / 1000),
          retired: false,
          chats: [],
        };
        this.leases.set(lease.id, lease);
        return this.json(res, 201, {
          lease_id: lease.id,
          lease_token: lease.token,
          client_request_id: lease.clientRequestId,
          bound: bound.toString(),
          issued_at: lease.issuedAt,
          expires_at: lease.issuedAt + 3600,
        });
      }

      if (req.method === 'POST' && url.pathname === '/v1/chat/completions')
        return this.chat(req, res, body);
      if (req.method === 'POST' && url.pathname === '/v2/withdraw/clearance')
        return this.clearance(res, body);
      if (req.method === 'POST' && url.pathname === '/v1/close')
        return await this.closeRoute(res, body);

      const leasePath = /^\/v2\/openrouter\/leases\/([^/]+)$/.exec(url.pathname);
      if (leasePath) {
        const lease = this.leases.get(leasePath[1]!);
        if (!this.authorize(req, res, lease)) return;
        if (req.method === 'POST') {
          lease.retired = true;
          return this.json(res, 200, { status: 'retiring' });
        }
        return this.json(res, 200, {
          status: lease.retired ? 'settled' : 'active',
          provider_evidence: { signatures: this.signatures(lease) },
        });
      }

      const spent = /^\/v2\/nullifiers\/([^/]+)$/.exec(url.pathname);
      if (spent && req.method === 'GET') {
        const lease = [...this.leases.values()].find((l) => l.nullifier === spent[1]);
        const status = !lease ? 'unknown' : lease.retired ? 'finalized' : 'reserved';
        return this.json(res, 200, { status, nullifier_status: status });
      }

      const recovery = /^\/v2\/requests\/([^/]+)$/.exec(url.pathname);
      if (recovery) {
        const lease = [...this.leases.values()].find((l) => l.clientRequestId === recovery[1]);
        if (!lease) return this.error(res, 404, 'lease_not_found');
        if (!lease.retired)
          return this.json(res, 200, { status: 'pending', nullifier_status: 'reserved' });
        const response = this.respond(lease);
        return this.json(
          res,
          200,
          null,
          `{"status":"finalized","nullifier_status":"finalized","request_response":${response}}`,
        );
      }
      this.error(res, 404, 'not_found');
    } catch (e) {
      this.error(res, 500, 'provider_error', e instanceof Error ? e.message : String(e));
    }
  }

  /** What the lease costs: the operator's formula over the usage it metered, plus any inflation. */
  charge(lease: Lease): bigint {
    const cost = lease.chats.reduce(
      (n, c) => n + costNano(modelById(c.model)!, BigInt(c.prompt), BigInt(c.completion)),
      0n,
    );
    const held = BigInt(Math.max(0, Math.floor(Date.now() / 1000) - lease.issuedAt));
    const base = chargeOf(lease.bound, cost, held, this.params());
    const inflated = base + BigInt(this.scenario.inflate);
    return inflated < lease.bound ? inflated : lease.bound;
  }

  private respond(lease: Lease): string {
    return op.test_operator_respond(
      1n,
      lease.requestJson,
      CHAIN_ID,
      contractHex(),
      this.charge(lease),
    );
  }

  // ---- clearance and close ----

  /** `POST /v2/withdraw/clearance`: a verified kind-2 proof gets a clearance from the clearance key. */
  private clearance(res: ServerResponse, body: string) {
    const s = this.scenario;
    if (s.emergency) return this.error(res, 503, 'emergency');
    if (s.clearanceError) return this.error(res, 409, s.clearanceError, s.clearanceError);
    const parts = splitObject(body);
    if (Object.keys(parts).sort().join() !== 'ownership_proof,withdrawal_nullifier')
      return this.error(res, 400, 'malformed_request', 'unexpected fields');
    const own = splitObject(parts.ownership_proof!);
    if (Object.keys(own).sort().join() !== 'proof,public_inputs')
      return this.error(res, 400, 'malformed_request', 'unexpected fields');
    const publicInputs = own.public_inputs!;
    const proof = (JSON.parse(own.proof!) as { proof: string }).proof;
    const inputs = splitObject(publicInputs);
    const nullifier = JSON.parse(inputs.withdrawal_nullifier!) as string;
    if (JSON.parse(parts.withdrawal_nullifier!) !== nullifier)
      return this.error(res, 400, 'malformed_request', 'the nullifier differs from the proof’s');
    if (Number(inputs.kind) !== 2 || Number(inputs.conceded) !== 0)
      return this.error(res, 422, 'proof_invalid', 'not an ownership proof');
    const now = Math.floor(Date.now() / 1000);
    const validUntil = Number(inputs.valid_until);
    if (validUntil < now || validUntil > now + 3300)
      return this.error(
        res,
        422,
        'proof_expired',
        'an ownership proof must end within 3,300 seconds',
      );
    if (!op.test_operator_verify_withdrawal(withdrawalVk, publicInputs, proof))
      return this.error(res, 422, 'proof_invalid', 'Groth16 verification failed');
    this.clearances.push(nullifier);
    this.json(
      res,
      200,
      null,
      op.test_operator_sign_clearance(2n, nullifier, CHAIN_ID, contractHex()),
    );
  }

  /** `POST /v1/close`: the closer's intake. The proof is verified; the chain effect is the test's. */
  private async closeRoute(res: ServerResponse, body: string) {
    if (this.scenario.closeError)
      return this.error(res, 429, this.scenario.closeError, 'limited', 30);
    const parsed = JSON.parse(body) as { proof?: string; inputs?: string[] };
    if (Object.keys(parsed).sort().join() !== 'inputs,proof' || !parsed.proof || !parsed.inputs)
      return this.error(res, 400, 'malformed_request', 'a close carries proof and inputs only');
    const decoded = decodeCall({ proof: parsed.proof, inputs: parsed.inputs });
    if (!decoded.valid || decoded.kind !== 1)
      return this.error(res, 422, 'proof_invalid', 'not a valid close proof');
    const record = { ...decoded, args: { proof: parsed.proof, inputs: parsed.inputs } };
    this.closes.push(record);
    await this.onClose?.(record);
    this.json(res, 200, { tx_hash: `operator-close-${this.closes.length}` });
  }

  // ---- chat ----

  private async chat(req: IncomingMessage, res: ServerResponse, body: string) {
    const s = this.scenario;
    const lease = this.leases.get(String(req.headers['x-nymira-lease-id'] ?? ''));
    if (!this.authorize(req, res, lease)) return;
    if (s.chatLimit === 'operator') return this.error(res, 429, 'operator_capacity', 'slots', 5);
    if (s.chatLimit === 'provider')
      return this.error(res, 429, 'provider_rate_limit', 'upstream', 7);
    const request = JSON.parse(body) as {
      model: string;
      stream?: boolean;
      max_tokens?: number;
      stream_options?: { include_usage?: boolean };
      messages: { role: string; content: string }[];
    };
    this.wire.push(body);
    const modelKeys = new ClientKeys(MODEL_SEED);
    const plain = request.messages.map((m) => ({
      role: m.role,
      content: decrypt(modelKeys, m.content),
    }));
    const clientPub = fromHex(String(req.headers['x-client-pub-key']));
    const last = plain[plain.length - 1]?.content ?? '';
    const reply = `echo: ${last}`;
    const prompt = plain.reduce((n, m) => n + new TextEncoder().encode(m.content).length, 0);
    const completion = reply.split(/\s+/).length;
    const chatId = `chatcmpl-${lease.id}-${lease.chats.length + 1}`;
    const seal = (text: string) => encrypt(clientPub, new TextEncoder().encode(text));
    const usage = {
      prompt_tokens: prompt,
      completion_tokens: completion,
      total_tokens: prompt + completion,
    };
    let raw: string;
    if (request.stream) {
      const chunk = (delta: unknown, extra: Record<string, unknown> = {}) =>
        `data: ${JSON.stringify({ id: chatId, object: 'chat.completion.chunk', model: request.model, choices: [{ index: 0, delta }], ...extra })}\n\n`;
      const words = reply.split(' ');
      raw =
        chunk({ role: 'assistant', content: '' }) +
        words.map((w, i) => chunk({ content: seal((i ? ' ' : '') + w) })).join('') +
        chunk({}, { choices: [], usage }) +
        'data: [DONE]\n\n';
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.end(raw);
    } else {
      raw = JSON.stringify({
        id: chatId,
        object: 'chat.completion',
        model: request.model,
        choices: [
          { index: 0, message: { role: 'assistant', content: seal(reply) }, finish_reason: 'stop' },
        ],
        usage,
      });
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(raw);
    }
    lease.chats.push({
      chatId,
      request: new TextEncoder().encode(body),
      response: new TextEncoder().encode(raw),
      model: request.model,
      prompt,
      completion,
      stream: Boolean(request.stream),
    });
  }

  private signatures(lease: Lease) {
    const s = this.scenario;
    const hash = (b: Uint8Array) => toHex(sha256(b));
    return lease.chats.flatMap((c) => {
      const kind =
        s.signature === 'auto'
          ? c.stream
            ? 'gateway'
            : 'provider_tee'
          : s.signature === 'bad'
            ? 'provider_tee'
            : s.signature;
      if (kind === 'none') return [];
      const gateway = kind === 'gateway';
      const text = gateway
        ? `${hash(c.request)}:${hash(c.response)}`
        : `${c.model}:${hash(c.request)}:${hash(c.response)}`;
      const seed = gateway ? GATEWAY_SEED : MODEL_SEED;
      let signature = toHex(ed25519.sign(new TextEncoder().encode(text), seed));
      if (s.signature === 'bad')
        signature = signature.replace(/^../, (b) => (b === '00' ? '01' : '00'));
      return [
        {
          chat_id: c.chatId,
          signature: {
            signature_kind: kind,
            text,
            signature,
            signing_address: toHex(ed25519.getPublicKey(seed)),
            signing_algo: 'ed25519',
          },
        },
      ];
    });
  }

  // ---- the gateway ----

  private evidence(role: 'model' | 'gateway', nonce: string, address: Uint8Array) {
    const s = this.scenario;
    const d = policy.deployments.find((x) => x.role === role)!;
    // A gateway that accepts any compose hash lists "*"; the test evidence then carries a made-up
    // 32-byte hash, which also runs the wildcard through the app's verifier.
    const composeHash = d.compose_hashes[0] === '*' ? 'ab'.repeat(32) : d.compose_hashes[0]!;
    const hex = (h: string) => fromHex(h);
    const events = [
      { imr: 3, event_type: 0x0800_0001, event: 'os-image-hash', event_payload: d.os_image_hash },
      { imr: 3, event_type: 0x0800_0001, event: 'compose-hash', event_payload: composeHash },
    ].map((e) => {
      const head = new Uint8Array(4);
      new DataView(head.buffer).setUint32(0, e.event_type, true);
      const digest = sha384(
        new Uint8Array([
          ...head,
          0x3a,
          ...new TextEncoder().encode(e.event),
          0x3a,
          ...hex(e.event_payload),
        ]),
      );
      return { ...e, digest: toHex(digest) };
    });
    let rtmr3 = new Uint8Array(48);
    for (const e of events) rtmr3 = sha384(new Uint8Array([...rtmr3, ...hex(e.digest)]));
    const appCompose = JSON.stringify({ manifest_version: 2, name: `test-${role}` });
    const config = new Uint8Array(48);
    config[0] = 1;
    config.set(sha256(new TextEncoder().encode(appCompose)), 1);
    const bound = role === 'model' && s.unboundKey ? new Uint8Array(32).fill(0xee) : address;
    const reportData = new Uint8Array(64);
    reportData.set(bound, 0);
    reportData.set(hex(nonce), 32);
    const facts = {
      kind: 'nymira-test-quote',
      status: s.tcb,
      advisory_ids: [],
      debug: false,
      mr_td: d.mr_td,
      rtmr0: d.rtmr0,
      rtmr1: d.rtmr1,
      rtmr2: d.rtmr2,
      rtmr3: toHex(rtmr3),
      mr_config_id: toHex(config),
      report_data: toHex(reportData),
    };
    return {
      signing_address: toHex(address),
      signing_algo: 'ed25519',
      intel_quote: toHex(new TextEncoder().encode(JSON.stringify(facts))),
      event_log: events,
      report_data: toHex(reportData),
      request_nonce: nonce,
      info: { tcb_info: { app_compose: appCompose } },
    };
  }

  private async gateway(req: IncomingMessage, res: ServerResponse) {
    this.cors(res, req);
    if (req.method === 'OPTIONS') return void res.writeHead(204).end();
    const url = new URL(req.url ?? '/', 'http://x');
    this.seen.push({ method: req.method ?? '', path: url.pathname, headers: { ...req.headers } });
    if (url.pathname !== '/v1/attestation/report') return this.error(res, 404, 'not_found');
    const nonce = url.searchParams.get('nonce') ?? '';
    const model = url.searchParams.get('model');
    const gateway = this.evidence('gateway', nonce, gatewayKey);
    if (!model) return this.json(res, 200, { gateway_attestation: gateway });
    const entry = {
      model_name: model,
      ...this.evidence('model', nonce, modelKey),
      signing_public_key: toHex(modelKey),
      nvidia_payload: JSON.stringify({ nonce, evidence_list: [] }),
    };
    this.json(res, 200, { gateway_attestation: gateway, model_attestations: [entry] });
  }
}

function contractHex(): string {
  return `0x${toHex(contractAddress(vault.vaultAccount))}`;
}
