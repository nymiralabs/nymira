// The server against nymirad's real agent routes in the mock stack (sandbox vault, real operator,
// fake NEAR AI), with funded budget notes and signed policies. Started by the Rust fixture
// `nymira-mcp-fixture`; the client is the official MCP SDK client over stdio.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { connect, lsof, startFixture, type Fixture, type Session } from "./support.ts";

let fx: Fixture;
before(async () => {
  fx = await startFixture();
});
after(() => fx?.stop());

const USD = /^[0-9]+\.[0-9]{6}$/;
const tcp = (file: string) => ["--token-file", file, "--transport", "tcp", "--url", `http://127.0.0.1:${fx.info.port}`];
const unix = (file: string) => ["--token-file", file, "--transport", "unix", "--socket", fx.info.socket];

async function call(s: Session, name: string, args: Record<string, unknown> = {}) {
  const r = await s.client.callTool({ name, arguments: args });
  const text = (r.content as { type: string; text: string }[])[0]!.text;
  return { r, text, body: JSON.parse(text) };
}

async function withSession<T>(args: string[], f: (s: Session) => Promise<T>): Promise<T> {
  const s = await connect(args);
  try {
    return await f(s);
  } finally {
    await s.close();
  }
}

test("nymira_balance returns the balance, limits and expiry as typed fields, in USD decimal strings", async () => {
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    const { r, text, body } = await call(s, "nymira_balance");
    assert.notEqual(r.isError, true, text);
    assert.deepEqual(r.structuredContent, body, "the text is a JSON rendering of the same object");
    assert.equal(text, JSON.stringify(r.structuredContent));
    assert.equal(body.balance_usd, "1.000000");
    assert.equal(body.spendable_usd, "1.000000");
    assert.equal(body.lease_bound_usd, "0.100000");
    assert.equal(body.spent_today_usd, "0.000000");
    assert.equal(body.note.status, "open");
    assert.equal(body.note.closing, false);
    assert.ok(Number.isInteger(body.note.expires_at) && body.note.expires_at > 0, String(body.note.expires_at));
    assert.equal(body.charge_warning, false);
    assert.deepEqual(body.limits, {
      max_call_usd: "0.050000", max_daily_usd: "2.000000", confirm_above_usd: "5.000000",
      max_lease_usd: "0.500000", allowed_models: [fx.info.model],
    });
    for (const k of ["balance_usd", "spendable_usd", "lease_bound_usd", "spent_today_usd"]) assert.match(body[k], USD);
    for (const k of ["max_call_usd", "max_daily_usd", "confirm_above_usd", "max_lease_usd"]) assert.match(body.limits[k], USD);
  });
});

test("nymira_quote with messages: allow, with the worst-case cost nymirad computed", async () => {
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    const { r, text, body } = await call(s, "nymira_quote", {
      model: fx.info.model, max_usd: "0.002", messages: [{ role: "user", content: "hello" }],
    });
    assert.notEqual(r.isError, true, text);
    assert.deepEqual(r.structuredContent, body);
    assert.equal(body.decision, "allow");
    assert.equal(body.reason, null);
    assert.equal(body.model, fx.info.model);
    assert.equal(body.input_tokens, 4 + 5, "role and content bytes, as nymirad counts them");
    assert.match(body.estimated_cost_usd, USD);
    assert.ok(Number(body.estimated_cost_usd) > 0 && Number(body.estimated_cost_usd) <= 0.002);
    assert.ok(Number.isInteger(body.max_tokens) && body.max_tokens > 0);
  });
});

test("nymira_quote with an input-token estimate asks nymirad for exactly that many input tokens", async () => {
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    const { r, text, body } = await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.01", input_tokens: 1000 });
    assert.notEqual(r.isError, true, text);
    assert.equal(body.decision, "allow");
    assert.equal(body.input_tokens, 1000);
    const more = (await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.01", input_tokens: 2000 })).body;
    assert.equal(more.input_tokens, 2000);
    assert.ok(more.max_tokens < body.max_tokens, "more input leaves fewer output tokens under the same cap");
  });
});

test("nymira_quote returns deny with a reason code, and needs_approval, from nymirad's decision", async () => {
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    const notAllowed = (await call(s, "nymira_quote", { model: "z-ai/other", max_usd: "0.002", input_tokens: 10 })).body;
    assert.equal(notAllowed.decision, "deny");
    assert.equal(notAllowed.reason, "model_not_allowed");
    assert.equal(notAllowed.http_status, 402);
    const tooSmall = (await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.000001", input_tokens: 100 })).body;
    assert.equal(tooSmall.decision, "deny");
    assert.equal(tooSmall.reason, "call_limit");
  });
  await withSession(tcp(fx.info.token_files.tight), async (s) => {
    const { r, body } = await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.002", input_tokens: 100 });
    assert.notEqual(r.isError, true);
    assert.equal(body.decision, "needs_approval");
    assert.equal(body.reason, "approval_required");
    assert.match(body.estimated_cost_usd, USD);
  });
});

test("over the Unix socket, with a token made for it", async () => {
  await withSession(unix(fx.info.token_files.sock), async (s) => {
    const bal = await call(s, "nymira_balance");
    assert.notEqual(bal.r.isError, true, bal.text);
    assert.equal(bal.body.balance_usd, "1.000000");
    const q = await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.002", input_tokens: 50 });
    assert.equal(q.body.decision, "allow");
    // No listening socket here either: the client end of the Unix socket has a peer.
    const inet = lsof(s.pid, "-i");
    assert.equal(/LISTEN|UDP/.test(inet), false, inet);
    assert.equal(/^n\//m.test(lsof(s.pid, "-U").replace(/n[^\n]*->[^\n]*/g, "")), false);
  });
});

test("a token made for the other transport is refused by nymirad and reported as such", async () => {
  await withSession(unix(fx.info.token_files.main), async (s) => {
    const { r, body } = await call(s, "nymira_balance");
    assert.equal(r.isError, true);
    assert.equal(body.error.code, "wrong_transport");
  });
  await withSession(tcp(fx.info.token_files.sock), async (s) => {
    const { r, body } = await call(s, "nymira_balance");
    assert.equal(r.isError, true);
    assert.equal(body.error.code, "wrong_transport");
  });
});

test("an owner token never starts the server, even from nymirad's own owner.token file", async () => {
  const { runToExit } = await import("./support.ts");
  const r = runToExit(tcp(fx.info.owner_token_file));
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /owner token/);
});

test("balance and quote open no lease and cause no outbound connection", async () => {
  const before = await fx.report();
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    for (let i = 0; i < 3; i++) {
      await call(s, "nymira_balance");
      await call(s, "nymira_quote", { model: fx.info.model, max_usd: "0.002", input_tokens: 100 });
    }
  });
  const after = await fx.report();
  assert.deepEqual([after.leases, after.chats, after.outbound], [0, 0, 0]);
  assert.deepEqual(after, before);
  // And the balance is unchanged afterwards.
  await withSession(tcp(fx.info.token_files.main), async (s) => {
    assert.equal((await call(s, "nymira_balance")).body.balance_usd, "1.000000");
  });
});
