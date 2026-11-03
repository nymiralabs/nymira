#!/usr/bin/env node
// Alerts when DeployContract, AddKey or DeleteKey touches one NEAR account (the vault).
//
//   node scripts/watch-vault-keys.mjs --account vault.nymira.testnet \
//        --rpc https://rpc.testnet.near.org,https://rpc.testnet.fastnear.com [--state watch.json]
//        [--expect pins.json] [--interval 30] [--once] [--no-events]
//   node scripts/watch-vault-keys.mjs --account ... --rpc ... --print   # current pins, to review and save
//
// Each alert is one JSON line on stdout. With --once the exit code is 2 when anything alerted and 1
// when the node could not be read. Without it the script keeps polling; pipe stdout to whatever
// notifies you. Silence is not an all-clear: after --fail-limit failed polls (default 3) it raises
// `rpc_unreachable` once per outage.
//
// How it sees the three actions:
//  - DeployContract: `contract_code_changes` of every block since the last poll, and the code hash.
//  - DeleteKey: `access_key_deletion` events, and any key missing from the key list.
//  - AddKey: any key in the key list that was not there, or whose permission differs. Nonce and
//    allowance move with every transaction a function-call key sends, so they are not compared.
// A key that was added and removed between two polls shows up as a deletion event.

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { pathToFileURL } from "node:url";

const MAX_BLOCKS_PER_TICK = 600;

async function rpc(urls, method, params, timeoutMs = 15000) {
  let last;
  for (const url of urls) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
        signal: AbortSignal.timeout(timeoutMs),
      });
      if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
      const body = await res.json();
      if (body.error) {
        const e = new Error(`${url}: ${body.error.name || body.error.message}`);
        e.rpcName = body.error?.cause?.name || body.error.name;
        throw e;
      }
      return body.result;
    } catch (e) {
      last = e;
      if (e.rpcName === "UNKNOWN_BLOCK") throw e; // a node answered: no other node will know it better
    }
  }
  throw last;
}

function permissionKey(permission) {
  if (permission === "FullAccess") return "FullAccess";
  const f = permission.FunctionCall;
  return JSON.stringify({ receiver_id: f.receiver_id, method_names: f.method_names });
}

async function readAccount(rpcs, account) {
  const [block, acct, list] = await Promise.all([
    rpc(rpcs, "block", { finality: "final" }),
    rpc(rpcs, "query", { request_type: "view_account", finality: "final", account_id: account }),
    rpc(rpcs, "query", { request_type: "view_access_key_list", finality: "final", account_id: account }),
  ]);
  const keys = {};
  for (const k of list.keys) keys[k.public_key] = permissionKey(k.access_key.permission);
  return { height: block.header.height, code_hash: acct.code_hash, keys };
}

async function scanEvents(rpcs, account, from, to) {
  const found = [];
  for (let h = from; h <= to; h++) {
    for (const changes_type of ["contract_code_changes", "all_access_key_changes"]) {
      let res;
      try {
        res = await rpc(rpcs, "EXPERIMENTAL_changes", { block_id: h, changes_type, account_ids: [account] });
      } catch (e) {
        if (e.rpcName === "UNKNOWN_BLOCK") continue; // a height with no block
        throw e;
      }
      for (const c of res.changes) {
        if (c.type === "contract_code_update" || c.type === "contract_code_deletion") {
          found.push({ alert: "deploy_contract", height: h, change: c.type, source: "event" });
        } else if (c.type === "access_key_deletion") {
          found.push({ alert: "delete_key", height: h, public_key: c.change.public_key, source: "event" });
        }
      }
    }
  }
  return found;
}

function diff(base, now, eventAlerts) {
  const out = [];
  const sawDeploy = eventAlerts.some((a) => a.alert === "deploy_contract");
  const deleted = new Set(eventAlerts.filter((a) => a.alert === "delete_key").map((a) => a.public_key));
  if (base.code_hash !== now.code_hash && !sawDeploy) {
    out.push({ alert: "deploy_contract", from: base.code_hash, to: now.code_hash, source: "state" });
  }
  for (const [pk, perm] of Object.entries(now.keys)) {
    if (!(pk in base.keys)) out.push({ alert: "add_key", public_key: pk, permission: perm });
    else if (base.keys[pk] !== perm) {
      out.push({ alert: "key_permission_changed", public_key: pk, from: base.keys[pk], to: perm });
    }
  }
  for (const pk of Object.keys(base.keys)) {
    if (!(pk in now.keys) && !deleted.has(pk)) out.push({ alert: "delete_key", public_key: pk, source: "state" });
  }
  return out;
}

const load = (p) => JSON.parse(readFileSync(p, "utf8"));
const save = (p, v) => writeFileSync(p, JSON.stringify(v, null, 2) + "\n");

/** One poll. Returns the alerts it raised; the state file is updated to what it saw. */
export async function tick({ rpcs, account, statePath, events = true, failLimit = 3, expectPath }) {
  const state = existsSync(statePath) ? load(statePath) : null;
  try {
    const now = await readAccount(rpcs, account);
    let alerts = [];
    if (!state) {
      const base = expectPath ? load(expectPath) : null;
      if (base) alerts = diff(base, now, []);
      save(statePath, { code_hash: now.code_hash, keys: now.keys, height: now.height, fails: 0, down: false });
      return alerts.map((a) => ({ account, ...a }));
    }
    const from = state.height + 1;
    const to = Math.min(now.height, state.height + MAX_BLOCKS_PER_TICK);
    const ev = events ? await scanEvents(rpcs, account, from, to) : [];
    alerts = [...ev, ...diff(state, now, ev)];
    if (events && to < now.height) alerts.push({ alert: "catching_up", scanned_to: to, final: now.height });
    save(statePath, { code_hash: now.code_hash, keys: now.keys, height: events ? to : now.height, fails: 0, down: false });
    return alerts.map((a) => ({ account, ...a }));
  } catch (e) {
    const next = { ...(state || { code_hash: null, keys: {}, height: 0 }) };
    next.fails = (next.fails || 0) + 1;
    const raise = next.fails >= failLimit && !next.down;
    if (raise) next.down = true;
    // A first run that cannot read the node has no baseline to protect: keep the empty state out.
    if (state) save(statePath, next);
    return raise ? [{ account, alert: "rpc_unreachable", failed_polls: next.fails, error: String(e.message) }] : [];
  }
}

function args(argv) {
  const o = { interval: 30, events: true, failLimit: 3 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--once") o.once = true;
    else if (a === "--print") o.print = true;
    else if (a === "--no-events") o.events = false;
    else if (a === "--account") o.account = argv[++i];
    else if (a === "--rpc") o.rpcs = argv[++i].split(",");
    else if (a === "--state") o.statePath = argv[++i];
    else if (a === "--expect") o.expectPath = argv[++i];
    else if (a === "--interval") o.interval = Number(argv[++i]);
    else if (a === "--fail-limit") o.failLimit = Number(argv[++i]);
    else throw new Error(`unknown argument ${a}`);
  }
  if (!o.account || !o.rpcs) throw new Error("--account and --rpc are required");
  o.statePath ||= `watch-${o.account}.json`;
  return o;
}

async function main() {
  const o = args(process.argv.slice(2));
  if (o.print) {
    const { code_hash, keys } = await readAccount(o.rpcs, o.account);
    console.log(JSON.stringify({ code_hash, keys }, null, 2));
    return;
  }
  let status = 0;
  for (;;) {
    const alerts = await tick(o);
    for (const a of alerts) console.log(JSON.stringify({ at: new Date().toISOString(), ...a }));
    if (o.once) {
      if (alerts.some((a) => a.alert === "rpc_unreachable")) status = 1;
      else if (alerts.length) status = 2;
      process.exit(status);
    }
    await new Promise((r) => setTimeout(r, o.interval * 1000));
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((e) => {
    console.error(e.message);
    process.exit(1);
  });
}
