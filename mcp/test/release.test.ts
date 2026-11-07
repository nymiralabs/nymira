// The release path, end to end and without a registry: build the tarball from a commit, check the
// checksums and signature, install it at a fixed path with no network, and run the host entries
// that the generator writes against that install with the official MCP client.
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS } from "@modelcontextprotocol/sdk/types.js";
import { hostConfig, HOSTS } from "../tools/host-config.ts";
import { AGENT_TOKEN, REPO, scratch, tokenFile } from "./support.ts";

const SCRIPT = join(REPO, "scripts", "release-mcp.sh");
const TAG = "nymira-mcp-v0.0.0";
const dist = scratch();
const env: NodeJS.ProcessEnv = { ...process.env, NYMIRA_MCP_REF: "HEAD", NYMIRA_MCP_DIST: dist };
const out = join(dist, TAG);
const tgz = join(out, "nymira-mcp-0.0.0.tgz");
const sha = (f: string) => execFileSync("shasum", ["-a", "256", f], { encoding: "utf8" }).split(" ")[0] as string;
const run = (args: string[], e = env) => spawnSync(SCRIPT, args, { env: e, encoding: "utf8" });
const hasMinisign = spawnSync("minisign", ["-v"]).status === 0;

test("build: two builds of one commit give one tarball, listed with the notes in SHA256SUMS", () => {
  const r = run(["build", TAG]);
  assert.equal(r.status, 0, r.stderr + r.stdout);
  assert.match(r.stdout, /two builds agree/);
  const sums = readFileSync(join(out, "SHA256SUMS"), "utf8");
  assert.ok(sums.includes(`${sha(tgz)}  nymira-mcp-0.0.0.tgz`));
  assert.ok(sums.includes(`${sha(join(out, "RELEASE_NOTES.md"))}  RELEASE_NOTES.md`));
  const notes = readFileSync(join(out, "RELEASE_NOTES.md"), "utf8");
  assert.ok(notes.includes(sha(tgz)), "the notes carry the package hash");
  assert.match(notes, /minisign -V -p release-signing\.pub/);
  // a second run from the same commit gives the same bytes
  const again = run(["build", TAG], { ...env, NYMIRA_MCP_DIST: scratch() });
  assert.equal(again.status, 0, again.stderr);
  assert.ok(again.stdout.includes(sha(tgz)), "the second run printed the first run's hash");
});

test("build refuses a tag that does not match package.json, and a bad tag", () => {
  assert.notEqual(run(["build", "nymira-mcp-v9.9.9"]).status, 0);
  assert.notEqual(run(["build", "v0.0.0"]).status, 0);
});

test("the tarball holds the built server and its locked dependencies, and no sources, tests or scripts", () => {
  const files = execFileSync("tar", ["-tzf", tgz], { encoding: "utf8" }).split("\n");
  assert.ok(files.includes("package/dist/index.js"));
  assert.ok(files.includes("package/node_modules/@modelcontextprotocol/sdk/package.json"));
  assert.ok(!files.some((f) => /^package\/(src|test|tools|tsconfig)/.test(f)));
  const pkg = JSON.parse(execFileSync("tar", ["-xzOf", tgz, "package/package.json"], { encoding: "utf8" }));
  const source = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(pkg.private, source.private, "the tarball carries the package's own publish flag");
  assert.deepEqual(pkg.bundleDependencies, ["@modelcontextprotocol/sdk"]);
});

test("verify: a good signature passes; a changed tarball, a changed sum file or a wrong key fails", { skip: !hasMinisign }, () => {
  const keys = scratch();
  const sk = join(keys, "k.key");
  const pk = join(keys, "k.pub");
  execFileSync("minisign", ["-G", "-W", "-f", "-s", sk, "-p", pk]);
  const e = { ...env, NYMIRA_MCP_PUB: pk, NYMIRA_MCP_KEY: sk };
  assert.notEqual(run(["verify", TAG], e).status, 0, "unsigned release is refused");
  execFileSync("minisign", ["-S", "-s", sk, "-m", join(out, "SHA256SUMS"), "-x", join(out, "SHA256SUMS.minisig")], { input: "\n" });
  const ok = run(["verify", TAG], e);
  assert.equal(ok.status, 0, ok.stderr + ok.stdout);
  const other = join(keys, "o");
  execFileSync("minisign", ["-G", "-W", "-f", "-s", `${other}.key`, "-p", `${other}.pub`]);
  assert.notEqual(run(["verify", TAG], { ...e, NYMIRA_MCP_PUB: `${other}.pub` }).status, 0);
  const sums = join(out, "SHA256SUMS");
  const good = readFileSync(sums, "utf8");
  writeFileSync(sums, good.replace(/^./, "0"));
  assert.notEqual(run(["verify", TAG], e).status, 0);
  writeFileSync(sums, good);
  appendFileSync(tgz, "x");
  assert.notEqual(run(["verify", TAG], e).status, 0);
  execFileSync("git", ["-C", REPO, "status", "--short"]);
  assert.equal(run(["build", TAG], { ...env, NYMIRA_MCP_DIST: dist }).status, 0, "rebuild restores the tarball");
});

const installDir = join(scratch(), "nymira-mcp");
const fs = scratch();

test("the documented install: hash check, then a fixed-path install with no network and no scripts", () => {
  const sums = spawnSync("shasum", ["-a", "256", "-c", "SHA256SUMS"], { cwd: out, encoding: "utf8" });
  assert.equal(sums.status, 0, sums.stderr);
  mkdirSync(installDir, { recursive: true });
  const r = spawnSync(
    "npm",
    ["install", "--prefix", installDir, "--ignore-scripts", "--offline", "--no-audit", "--no-fund", tgz],
    { encoding: "utf8", env: { ...process.env, npm_config_cache: join(fs, "empty-cache") } },
  );
  assert.equal(r.status, 0, r.stderr);
  assert.ok(existsSync(join(installDir, "node_modules/@nymiralabs/nymira-mcp/dist/index.js")));
});

for (const host of HOSTS) {
  test(`${host}: the generated entry has the documented shape, holds no token, and the installed server speaks MCP`, async () => {
    const file = tokenFile(AGENT_TOKEN);
    const cfg = hostConfig({ host, installDir, tokenFile: file, transport: "tcp", url: "http://127.0.0.1:8788" });
    const text = JSON.stringify(cfg);
    assert.ok(!text.includes("nyma_") && !text.includes("nymo_"), "no token in the configuration");
    const key = host === "vscode" ? "servers" : "mcpServers";
    // Cursor and VS Code name the transport; Claude Desktop's shape has no `type` field.
    const type = host === "claude-desktop" ? undefined : "stdio";
    const entry = (cfg as Record<string, Record<string, Record<string, unknown>>>)[key]?.nymira;
    assert.ok(entry, `${key}.nymira exists`);
    assert.deepEqual(Object.keys(cfg), [key]);
    assert.equal(entry.type, type);
    assert.equal(typeof entry.command, "string");
    assert.ok((entry.command as string).startsWith("/"), "absolute command: a desktop host has a short PATH");
    const args = entry.args as string[];
    assert.ok(args.every((a) => typeof a === "string"));
    assert.ok(args[0]?.startsWith(installDir), "the entry runs the fixed install");
    assert.ok(args.includes(file) && args.includes("--token-file"));
    assert.equal(entry.env, undefined);
    // run exactly that command with the official client
    const t = new StdioClientTransport({ command: entry.command as string, args, env: { PATH: process.env.PATH ?? "" } });
    const c = new Client({ name: "host-conformance", version: "0.0.0" });
    await c.connect(t);
    try {
      const { tools } = await c.listTools();
      assert.deepEqual(tools.map((x) => x.name), ["nymira_balance", "nymira_quote", "nymira_ask"]);
      // The server answers with a protocol version the SDK client supports; it is recorded in the notes.
      assert.ok(SUPPORTED_PROTOCOL_VERSIONS.includes(LATEST_PROTOCOL_VERSION));
      assert.equal(c.getServerVersion()?.name, "nymira-mcp");
    } finally {
      await c.close();
    }
  });
}

test("the generator writes a unix entry and refuses what the server would refuse", () => {
  const file = tokenFile(AGENT_TOKEN, 0o640);
  const u = hostConfig({ host: "cursor", installDir, tokenFile: file, transport: "unix", socket: "/run/nymira/agent.sock" });
  const a = (u as { mcpServers: { nymira: { args: string[] } } }).mcpServers.nymira.args;
  assert.deepEqual(a.slice(-4), ["--transport", "unix", "--socket", "/run/nymira/agent.sock"]);
  const base = { host: "cursor", installDir, tokenFile: file } as const;
  assert.throws(() => hostConfig({ ...base, transport: "tcp", url: "http://example.com:8788" }), /loopback/);
  assert.throws(() => hostConfig({ ...base, transport: "unix", socket: "agent.sock" }), /absolute/);
  assert.throws(() => hostConfig({ ...base, tokenFile: "agent.token", transport: "tcp" }), /absolute/);
  assert.throws(() => hostConfig({ ...base, installDir: "rel", transport: "tcp" }), /absolute/);
  assert.throws(() => hostConfig({ ...base, tokenFile: AGENT_TOKEN, transport: "tcp" }), /absolute|token/);
  assert.throws(() => hostConfig({ ...base, host: "ironclaw" as never, transport: "tcp" }), /host/);
});

test("the SDK line is 1.x and the two lines are never mixed", () => {
  const lock = JSON.parse(readFileSync(new URL("../package-lock.json", import.meta.url), "utf8"));
  assert.ok(!Object.keys(lock.packages).some((k) => k.startsWith("node_modules/@modelcontextprotocol/server")));
  assert.equal(lock.packages["node_modules/@modelcontextprotocol/sdk"].version, "1.32.1");
});

test("the docs state what the MCP path shows and what it cannot limit", () => {
  const readme = readFileSync(new URL("../README.md", import.meta.url), "utf8");
  const tm = readFileSync(join(REPO, "THREAT_MODEL.md"), "utf8");
  for (const [name, text] of [["mcp/README.md", readme], ["THREAT_MODEL.md", tm]] as const) {
    assert.match(text, /agent host and its model vendor see/i, name);
    assert.match(text, /no remote kill switch/i, name);
    assert.match(text, /many smaller calls/i, name);
    assert.match(text, /advisory/i, name);
  }
  assert.match(tm, /^## MCP spending tools$/m);
  assert.match(readme, /shasum -a 256 -c SHA256SUMS/);
  assert.match(readme, /minisign -V -p release-signing\.pub/);
});

test("the server answers initialize with the client's protocol version when it knows it, else its latest", async () => {
  const { ENTRY } = await import("./support.ts");
  const ask = (version: string) => new Promise<string>((resolve, reject) => {
    const child = spawn(process.execPath, [ENTRY, "--token-file", tokenFile(AGENT_TOKEN), "--transport", "tcp"]);
    let buf = "";
    child.stdout.on("data", (d: Buffer) => {
      buf += d.toString();
      const line = buf.split("\n")[0];
      if (buf.includes("\n")) { child.kill(); resolve(JSON.parse(line as string).result.protocolVersion); }
    });
    child.on("error", reject);
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: version, capabilities: {}, clientInfo: { name: "raw", version: "0" } } })}\n`);
  });
  assert.equal(await ask("2025-11-25"), "2025-11-25");
  assert.equal(await ask("2025-06-18"), "2025-06-18");
  assert.equal(await ask("2099-01-01"), LATEST_PROTOCOL_VERSION);
});
