// Run: node --test scripts/watch-vault-keys.test.mjs
// A stub JSON-RPC node stands in for NEAR; the watcher reads it through its real code path.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtempSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { tick, poll } from "./watch-vault-keys.mjs";

const ACCOUNT = "vault.test.near";
const OPERATOR_KEY = {
  public_key: "ed25519:op",
  access_key: {
    nonce: 1,
    permission: { FunctionCall: { allowance: "1000", receiver_id: ACCOUNT, method_names: ["heartbeat"] } },
  },
};

// A scripted chain: tests change `chain` between ticks.
function newChain() {
  return {
    height: 100,
    code_hash: "CODE1",
    keys: [OPERATOR_KEY],
    events: {}, // height -> { contract_code_changes: [...], all_access_key_changes: [...] }
    down: false,
  };
}

async function stub(chain) {
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      if (chain.down) return res.writeHead(503).end("down");
      const { method, params, id } = JSON.parse(body);
      const ok = (result) => res.end(JSON.stringify({ jsonrpc: "2.0", id, result }));
      if (method === "block") return ok({ header: { height: chain.height } });
      if (method === "query" && params.request_type === "view_account") return ok({ code_hash: chain.code_hash });
      if (method === "query" && params.request_type === "view_access_key_list") return ok({ keys: chain.keys });
      if (method === "query" && params.request_type === "call_function") {
        const value = chain.proposals?.[params.method_name];
        if (value === undefined) return res.end(JSON.stringify({ jsonrpc: "2.0", id, error: { name: "METHOD_NOT_FOUND" } }));
        return ok({ result: [...Buffer.from(JSON.stringify(value))] });
      }
      if (method === "EXPERIMENTAL_changes") {
        const named = chain.blockErrors?.[params.block_id];
        if (named) {
          const cause = { name: named };
          return res.end(JSON.stringify({ jsonrpc: "2.0", id, error: { name: "HANDLER_ERROR", cause } }));
        }
        const at = chain.events[params.block_id];
        return ok({ block_hash: "h", changes: (at && at[params.changes_type]) || [] });
      }
      res.end(JSON.stringify({ jsonrpc: "2.0", id, error: { name: "METHOD_NOT_FOUND" } }));
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { server, url: `http://127.0.0.1:${server.address().port}` };
}

async function setup(ctx) {
  const chain = newChain();
  const { server, url } = await stub(chain);
  const statePath = join(mkdtempSync(join(tmpdir(), "watch-")), "state.json");
  ctx.after(() => { server.closeAllConnections?.(); server.close(); });
  const opts = { rpcs: [url], account: ACCOUNT, statePath, events: true, failLimit: 3 };
  const run = () => tick(opts);
  return { chain, run, statePath, opts };
}

test("the first run records a baseline and alerts on nothing", async (ctx) => {
  const t = await setup(ctx);
  assert.deepEqual(await t.run(), []);
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).code_hash, "CODE1");
});

test("a quiet chain stays quiet, also when a function-call key spends allowance and its nonce moves", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 130;
  t.chain.keys = [{ ...OPERATOR_KEY, access_key: { nonce: 9, permission: { FunctionCall: { allowance: "400", receiver_id: ACCOUNT, method_names: ["heartbeat"] } } } }];
  t.chain.events[120] = { all_access_key_changes: [{ type: "access_key_update", change: { public_key: "ed25519:op" } }] };
  assert.deepEqual(await t.run(), []);
});

test("DeployContract: a code update event raises one deploy alert", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.code_hash = "CODE2";
  t.chain.events[105] = { contract_code_changes: [{ type: "contract_code_update", change: { account_id: ACCOUNT } }] };
  const alerts = await t.run();
  assert.deepEqual(alerts.map((a) => a.alert), ["deploy_contract"]);
  assert.equal(alerts[0].height, 105);
  assert.deepEqual(await t.run(), [], "the same change is not reported twice");
});

test("AddKey: a new key, and a key whose permission widened, each raise an alert", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.keys = [OPERATOR_KEY, { public_key: "ed25519:new", access_key: { nonce: 0, permission: "FullAccess" } }];
  let alerts = await t.run();
  assert.deepEqual(alerts.map((a) => [a.alert, a.public_key, a.permission]), [["add_key", "ed25519:new", "FullAccess"]]);

  t.chain.height = 120;
  t.chain.keys = [
    { ...OPERATOR_KEY, access_key: { nonce: 3, permission: "FullAccess" } },
    t.chain.keys[1],
  ];
  alerts = await t.run();
  assert.deepEqual(alerts.map((a) => [a.alert, a.public_key]), [["key_permission_changed", "ed25519:op"]]);
});

test("DeleteKey: the event raises an alert, and a deletion that happened between two ticks is seen", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.events[108] = {
    all_access_key_changes: [{ type: "access_key_deletion", change: { public_key: "ed25519:gone" } }],
  };
  const alerts = await t.run();
  assert.deepEqual(alerts.map((a) => [a.alert, a.public_key]), [["delete_key", "ed25519:gone"]]);
});

test("a key removed with no event in range (a gap) is still caught from the key list", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.keys = [];
  const alerts = await t.run();
  assert.deepEqual(alerts.map((a) => [a.alert, a.public_key]), [["delete_key", "ed25519:op"]]);
});

test("silence is not an all-clear: three failed ticks raise one rpc_unreachable alert, and recovery re-arms it", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.down = true;
  assert.deepEqual(await t.run(), []);
  assert.deepEqual(await t.run(), []);
  assert.deepEqual((await t.run()).map((a) => a.alert), ["rpc_unreachable"]);
  assert.deepEqual(await t.run(), [], "one alert per outage");
  t.chain.down = false;
  assert.deepEqual(await t.run(), []);
  t.chain.down = true;
  await t.run();
  await t.run();
  assert.deepEqual((await t.run()).map((a) => a.alert), ["rpc_unreachable"]);
});

test("a pinned expectation file replaces the first-run baseline, so a vault that already differs alerts at once", async (ctx) => {
  const chain = newChain();
  const { server, url } = await stub(chain);
  ctx.after(() => { server.closeAllConnections?.(); server.close(); });
  const dir = mkdtempSync(join(tmpdir(), "watch-"));
  const expectPath = join(dir, "expect.json");
  writeFileSync(expectPath, JSON.stringify({ code_hash: "PINNED", keys: {} }));
  const alerts = await tick({ rpcs: [url], account: ACCOUNT, statePath: join(dir, "s.json"), events: false, failLimit: 3, expectPath });
  assert.deepEqual(alerts.map((a) => a.alert).sort(), ["add_key", "deploy_contract"]);
});

test("a watcher that never worked is not silent: failed first polls are counted and raise rpc_unreachable", async (ctx) => {
  const t = await setup(ctx);
  t.chain.down = true;
  const opts = { ...t.opts, failLimit: 2 };
  assert.deepEqual(await poll(opts), { alerts: [], failed: true });
  const second = await poll(opts);
  assert.deepEqual(second.alerts.map((a) => a.alert), ["rpc_unreachable"]);
  assert.equal(second.failed, true);
  t.chain.down = false;
  assert.deepEqual(await t.run(), [], "the first poll that works records the baseline");
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).code_hash, "CODE1");
});

test("a corrupt state file is reported and left untouched, not overwritten", async (ctx) => {
  const t = await setup(ctx);
  writeFileSync(t.statePath, '{"code_hash": "CO');
  const r = await poll(t.opts);
  assert.deepEqual(r.alerts.map((a) => a.alert), ["state_unreadable"]);
  assert.equal(r.failed, true);
  assert.equal(readFileSync(t.statePath, "utf8"), '{"code_hash": "CO');
});

test("catching up scans in steps, keeps one consistent snapshot and reports a change once", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 1000; // 900 blocks since the last poll: more than one scan step
  t.chain.code_hash = "CODE2";
  t.chain.events[650] = { contract_code_changes: [{ type: "contract_code_update", change: { account_id: ACCOUNT } }] };
  let alerts = await t.run();
  assert.deepEqual(alerts.map((a) => a.alert), ["deploy_contract", "catching_up"]);
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).code_hash, "CODE1", "the snapshot stays old until the scan is done");
  assert.deepEqual(await t.run(), [], "the deploy seen in the first step is not reported again by the final diff");
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).code_hash, "CODE2");
});

test("an unfamiliar permission shape is compared, not a crash that looks like an outage", async (ctx) => {
  const t = await setup(ctx);
  t.chain.keys = [{ public_key: "ed25519:x", access_key: { nonce: 0, permission: { Delegated: { who: "y" } } } }];
  await t.run();
  t.chain.keys = [{ public_key: "ed25519:x", access_key: { nonce: 0, permission: { Delegated: { who: "z" } } } }];
  const alerts = await t.run();
  assert.deepEqual(alerts.map((a) => a.alert), ["key_permission_changed"]);
});

test("a block the node has pruned fails the poll instead of being skipped", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.blockErrors = { 105: "GARBAGE_COLLECTED_BLOCK" };
  const r = await poll(t.opts);
  assert.equal(r.failed, true);
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).height, 100, "the height does not move past blocks that were not scanned");
});

test("a height with no block (UNKNOWN_BLOCK on every node) is skipped", async (ctx) => {
  const t = await setup(ctx);
  await t.run();
  t.chain.height = 110;
  t.chain.blockErrors = { 105: "UNKNOWN_BLOCK" };
  const r = await poll(t.opts);
  assert.equal(r.failed, false);
  assert.equal(JSON.parse(readFileSync(t.statePath, "utf8")).height, 110);
});

test("an owner proposal that appears, changes or goes is an alert; a vault without the views is quiet", async (ctx) => {
  const t = await setup(ctx);
  t.chain.proposals = { pending_tvl_cap: null, pending_params: null };
  await t.run();
  t.chain.height = 110;
  assert.deepEqual(await t.run(), []);
  t.chain.height = 120;
  t.chain.proposals.pending_tvl_cap = { tvl_cap: "200000000", effective_at: "1", lapses_at: "2" };
  const alerts = await t.run();
  assert.equal(alerts.length, 1);
  assert.equal(alerts[0].alert, "owner_proposal_changed");
  assert.equal(alerts[0].kind, "tvl_cap");
  t.chain.height = 130;
  assert.deepEqual(await t.run(), []);
  t.chain.height = 140;
  t.chain.proposals.pending_tvl_cap = null;
  assert.equal((await t.run())[0].kind, "tvl_cap");
  // A vault with neither view: both read "n/a" and never differ.
  const quiet = await setup(ctx);
  await quiet.run();
  quiet.chain.height = 110;
  assert.deepEqual(await quiet.run(), []);
});
