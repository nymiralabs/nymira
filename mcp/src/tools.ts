// The two tools. Names, descriptions, schemas and annotations are fixed here and compared with
// test/tools.snapshot.json by a test. Nothing is derived from configuration or from nymirad.
import { isPositiveUsd, limitToUsd, MAX_USD_PATTERN, microToUsd, UpstreamError } from "./amounts.ts";
import { TransportError, type Fetcher } from "./nymirad.ts";

export const BALANCE_PATH = "/v1/nymira/balance";
export const QUOTE_PATH = "/v1/nymira/quote";
/** nymirad sets no limit of its own on a chat body, so the framework default of 2 MiB applies. */
export const MAX_BODY_BYTES = 2 * 1024 * 1024;
export const MAX_MESSAGES = 1024;
const MAX_INPUT_TOKENS = 1_000_000;
// The bytes a one-message request spends on its own fields: `user` counts as 4 input tokens.
const ROLE_TOKENS = 4;

const ANNOTATIONS = {
  readOnlyHint: true,
  destructiveHint: false,
  idempotentHint: true,
  openWorldHint: false,
} as const;

const USD = { type: "string", pattern: "^[0-9]+\\.[0-9]{6}$" } as const;
const CODE = { type: "string", pattern: "^[a-z][a-z0-9_]{0,63}$" } as const;

export const TOOLS = [
  {
    name: "nymira_balance",
    title: "Nymira balance",
    description:
      "Read this agent's prepaid Nymira budget: balance, spendable amount, today's spend, expiry, whether the note is closing, and the owner's limits. Amounts are USD decimal strings with six places. Read-only; spends nothing.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    outputSchema: {
      type: "object",
      properties: {
        balance_usd: USD,
        spendable_usd: USD,
        lease_bound_usd: USD,
        spent_today_usd: USD,
        note: {
          type: "object",
          properties: {
            status: { enum: ["open", "expiring", "revoking", "exiting", "closed", "unfunded"] },
            closing: { type: "boolean" },
            expires_at: { type: ["integer", "null"] },
          },
          required: ["status", "closing", "expires_at"],
          additionalProperties: false,
        },
        charge_warning: { type: "boolean" },
        limits: {
          type: "object",
          properties: {
            max_call_usd: USD,
            max_daily_usd: USD,
            confirm_above_usd: USD,
            max_lease_usd: USD,
            allowed_models: { type: "array", items: { type: "string" } },
          },
          required: ["max_call_usd", "max_daily_usd", "confirm_above_usd", "max_lease_usd", "allowed_models"],
          additionalProperties: false,
        },
      },
      required: ["balance_usd", "spendable_usd", "lease_bound_usd", "spent_today_usd", "note", "charge_warning", "limits"],
      additionalProperties: false,
    },
    annotations: { title: "Nymira balance", ...ANNOTATIONS },
  },
  {
    name: "nymira_quote",
    title: "Nymira quote",
    description:
      "Ask whether a model call would be allowed, without making it. Give a model, either messages (user and assistant roles) or an input_tokens estimate, and max_usd, the most the call may cost as a USD decimal with at most six places. Returns the estimated worst-case cost and a decision: allow, needs_approval or deny with a reason code. Read-only; opens no lease and sends nothing.",
    inputSchema: {
      type: "object",
      properties: {
        model: { type: "string", minLength: 1, maxLength: 128 },
        messages: {
          type: "array",
          minItems: 1,
          maxItems: MAX_MESSAGES,
          items: {
            type: "object",
            properties: {
              role: { enum: ["user", "assistant"] },
              content: { type: "string" },
            },
            required: ["role", "content"],
            additionalProperties: false,
          },
        },
        input_tokens: { type: "integer", minimum: ROLE_TOKENS + 1, maximum: MAX_INPUT_TOKENS },
        max_usd: { type: "string", pattern: MAX_USD_PATTERN },
      },
      required: ["model", "max_usd"],
      additionalProperties: false,
    },
    outputSchema: {
      type: "object",
      properties: {
        decision: { enum: ["allow", "needs_approval", "deny"] },
        reason: { anyOf: [CODE, { type: "null" }] },
        model: { type: "string" },
        input_tokens: { type: "integer" },
        max_tokens: { type: "integer" },
        estimated_cost_usd: USD,
        http_status: { type: "integer" },
        retry_after_secs: { type: "integer" },
        detail: CODE,
      },
      required: ["decision", "reason"],
      additionalProperties: false,
    },
    annotations: { title: "Nymira quote", ...ANNOTATIONS },
  },
] as const;

export class InputError extends Error {}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

function strictKeys(v: Obj, allowed: string[], where: string): void {
  for (const k of Object.keys(v)) {
    if (!allowed.includes(k)) throw new InputError(`${where}: unknown field ${JSON.stringify(k.slice(0, 40))}`);
  }
}

export interface QuoteInput {
  model: string;
  max_usd: string;
  messages?: { role: "user" | "assistant"; content: string }[];
  input_tokens?: number;
}

/** Strict checks before anything reaches nymirad. Messages never appear in an error. */
export function parseQuoteInput(args: unknown): QuoteInput {
  if (!isObj(args)) throw new InputError("arguments must be an object");
  strictKeys(args, ["model", "messages", "input_tokens", "max_usd"], "arguments");
  const { model, messages, input_tokens, max_usd } = args;
  if (typeof model !== "string" || model.length < 1 || model.length > 128) {
    throw new InputError("model must be a string of 1 to 128 characters");
  }
  if (!isPositiveUsd(max_usd)) {
    throw new InputError("max_usd must be a positive USD decimal with at most six places, such as 0.002");
  }
  if ((messages === undefined) === (input_tokens === undefined)) {
    throw new InputError("give exactly one of messages and input_tokens");
  }
  if (input_tokens !== undefined) {
    if (!Number.isInteger(input_tokens) || (input_tokens as number) <= ROLE_TOKENS || (input_tokens as number) > MAX_INPUT_TOKENS) {
      throw new InputError(`input_tokens must be a whole number from ${ROLE_TOKENS + 1} to ${MAX_INPUT_TOKENS}`);
    }
    return { model, max_usd, input_tokens: input_tokens as number };
  }
  if (!Array.isArray(messages) || messages.length < 1 || messages.length > MAX_MESSAGES) {
    throw new InputError(`messages must be 1 to ${MAX_MESSAGES} items`);
  }
  const out: NonNullable<QuoteInput["messages"]> = [];
  messages.forEach((m: unknown, i) => {
    if (!isObj(m)) throw new InputError(`messages[${i}] must be an object`);
    strictKeys(m, ["role", "content"], `messages[${i}]`);
    if (m.role !== "user" && m.role !== "assistant") {
      throw new InputError(`messages[${i}].role must be user or assistant`);
    }
    if (typeof m.content !== "string") throw new InputError(`messages[${i}].content must be a string`);
    out.push({ role: m.role, content: m.content });
  });
  return { model, max_usd, messages: out };
}

/** The chat-shaped body nymirad's quote route takes, or an error if it would be too large. */
export function quoteBody(input: QuoteInput): string {
  const messages = input.messages ?? [{ role: "user", content: "a".repeat((input.input_tokens as number) - ROLE_TOKENS) }];
  const text = JSON.stringify({ model: input.model, stream: false, messages });
  if (Buffer.byteLength(text, "utf8") >= MAX_BODY_BYTES) {
    throw new InputError("the request is larger than nymirad accepts");
  }
  return text;
}

// ---- reading nymirad's answers ----

const NOTE_STATUS = ["open", "expiring", "revoking", "exiting", "closed", "unfunded"];
const WORD = /^[a-z][a-z0-9_]{0,63}$/;
const MODEL_ID = /^[A-Za-z0-9._/:-]{1,128}$/;

function word(v: unknown): string {
  if (typeof v !== "string" || !WORD.test(v)) throw new UpstreamError("a code was not a plain word");
  return v;
}
function int(v: unknown): number {
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw new UpstreamError("a count was not a whole number");
  return v;
}
function obj(v: unknown): Obj {
  if (!isObj(v)) throw new UpstreamError("an answer was not an object");
  return v;
}

export function shapeBalance(raw: unknown): Obj {
  const r = obj(raw);
  const note = obj(r.note);
  const limits = obj(r.limits);
  if (!NOTE_STATUS.includes(note.status as string)) throw new UpstreamError("unknown note status");
  if (typeof note.closing !== "boolean" || typeof r.charge_warning !== "boolean") {
    throw new UpstreamError("a flag was not a boolean");
  }
  const models = limits.allowed_models;
  if (!Array.isArray(models) || !models.every((m) => typeof m === "string" && MODEL_ID.test(m))) {
    throw new UpstreamError("allowed_models was malformed");
  }
  return {
    balance_usd: microToUsd(r.balance_micro),
    spendable_usd: microToUsd(r.spendable_micro),
    lease_bound_usd: microToUsd(r.lease_bound_micro),
    spent_today_usd: microToUsd(r.spent_today_micro),
    note: {
      status: note.status,
      closing: note.closing,
      expires_at: note.expires_at === null ? null : int(note.expires_at),
    },
    charge_warning: r.charge_warning,
    limits: {
      max_call_usd: limitToUsd(limits.max_call_usd),
      max_daily_usd: limitToUsd(limits.max_daily_usd),
      confirm_above_usd: limitToUsd(limits.confirm_above_usd),
      max_lease_usd: limitToUsd(limits.max_lease_usd),
      allowed_models: models,
    },
  };
}

export function shapeQuote(raw: unknown): Obj {
  const r = obj(raw);
  if (r.decision !== "allow" && r.decision !== "needs_approval" && r.decision !== "deny") {
    throw new UpstreamError("unknown decision");
  }
  const out: Obj = { decision: r.decision, reason: r.reason === null ? null : word(r.reason) };
  if (r.model !== undefined) {
    if (typeof r.model !== "string" || !MODEL_ID.test(r.model)) throw new UpstreamError("model was malformed");
    out.model = r.model;
  }
  if (r.input_tokens !== undefined) out.input_tokens = int(r.input_tokens);
  if (r.max_tokens !== undefined) out.max_tokens = int(r.max_tokens);
  if (r.estimated_cost_micro !== undefined) out.estimated_cost_usd = microToUsd(r.estimated_cost_micro);
  if (r.http_status !== undefined) out.http_status = int(r.http_status);
  if (r.retry_after_secs !== undefined) out.retry_after_secs = int(r.retry_after_secs);
  if (r.detail !== undefined) out.detail = word(r.detail);
  return out;
}

// ---- running a call ----

export interface ToolResult {
  [key: string]: unknown;
  content: { type: "text"; text: string }[];
  structuredContent?: Obj;
  isError?: boolean;
}

const ok = (value: Obj): ToolResult => ({
  content: [{ type: "text", text: JSON.stringify(value) }],
  structuredContent: value,
});

const fail = (code: string, message: string, status?: number): ToolResult => ({
  isError: true,
  content: [{ type: "text", text: JSON.stringify({ error: { code, message, ...(status ? { http_status: status } : {}) } }) }],
});

/** What nymirad refused with, as a code and a fixed sentence; its own text is never passed on. */
function refusal(status: number, body: unknown): ToolResult {
  const code = isObj(body) && isObj(body.error) && typeof body.error.code === "string" && WORD.test(body.error.code)
    ? body.error.code
    : "request_refused";
  const known: Record<string, string> = {
    invalid_token: "nymirad does not know this token",
    token_disabled: "the owner disabled this agent token",
    wrong_transport: "this token was made for the other transport; set --transport to match it",
    policy_missing: "the owner has not loaded a valid signed policy for this agent",
    invalid_request: "nymirad refused the request as malformed",
  };
  return fail(code, known[code] ?? "nymirad refused the request", status);
}

export async function runTool(fetcher: Fetcher, name: string, args: unknown): Promise<ToolResult> {
  try {
    if (name === "nymira_balance") {
      if (!isObj(args) || Object.keys(args).length > 0) throw new InputError("nymira_balance takes no arguments");
      const r = await fetcher("GET", BALANCE_PATH, {});
      return r.status === 200 ? ok(shapeBalance(r.body)) : refusal(r.status, r.body);
    }
    if (name === "nymira_quote") {
      const input = parseQuoteInput(args ?? {});
      const body = quoteBody(input);
      const r = await fetcher("POST", QUOTE_PATH, { "x-nymira-max-cost-usd": input.max_usd }, body);
      return r.status === 200 ? ok(shapeQuote(r.body)) : refusal(r.status, r.body);
    }
    return fail("unknown_tool", "no such tool");
  } catch (e) {
    if (e instanceof InputError) return fail("invalid_input", e.message);
    if (e instanceof TransportError) return fail(e.code, "could not get an answer from nymirad");
    if (e instanceof UpstreamError) return fail("unexpected_response", "nymirad's answer was not in the expected shape");
    return fail("internal_error", "the tool failed");
  }
}
