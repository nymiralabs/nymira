// The Cursor recipe in the README: its MCP entry is what the generator writes, holds no token, and
// the recipe says what Cursor and its model vendor see. IronClaw has no entry, and the docs say why.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { hostConfig } from "../tools/host-config.ts";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const readme = readFileSync(join(REPO, "README.md"), "utf8");
const mcpReadme = readFileSync(join(REPO, "mcp", "README.md"), "utf8");

const section = (title: string) => {
  const start = readme.indexOf(`### ${title}\n`);
  assert.ok(start >= 0, `README has a "${title}" section`);
  const end = readme.indexOf("\n### ", start + 1);
  return readme.slice(start, end < 0 ? undefined : end);
};

test("Cursor's entry is a stdio server with the install path and the token file, and no token", () => {
  const entry = hostConfig({
    host: "cursor", installDir: "/opt/nymira-mcp", tokenFile: "/srv/agent/cursor.token",
    transport: "tcp", node: "/usr/local/bin/node",
  }) as { mcpServers: { nymira: Record<string, unknown> } };
  const e = entry.mcpServers.nymira;
  assert.equal(e.type, "stdio");
  assert.equal(e.command, "/usr/local/bin/node");
  assert.ok((e.args as string[]).includes("/srv/agent/cursor.token"));
  assert.ok(!JSON.stringify(entry).match(/nyma[_o]/i), "no token in the entry");
});

test("the README's Cursor entry is the generator's output", () => {
  const text = section("Cursor");
  const block = /```json\n([\s\S]*?)```/.exec(text)?.[1];
  assert.ok(block, "the Cursor section has a JSON block");
  const shown = JSON.parse(block) as { mcpServers: { nymira: { command: string; args: string[] } } };
  const made = hostConfig({
    host: "cursor", installDir: "/opt/nymira-mcp", tokenFile: "/srv/agent/cursor.token",
    transport: "tcp", node: shown.mcpServers.nymira.command,
  });
  assert.deepEqual(shown, made);
  assert.ok(!/nyma[_o]/i.test(block));
});

test("the Cursor recipe says what Cursor sees, why the base URL override is not used, and how to check", () => {
  const text = section("Cursor");
  assert.match(text, /Cursor and its model vendor see/i);
  assert.match(text, /base URL override/i);
  assert.match(text, /forum\.cursor\.com\/t\/problem-reaching-openai-error-on-a-local-model-with-overriden-base-url\/3975/);
  assert.match(text, /forum\.cursor\.com\/t\/connecting-local-ai-server-to-cursor-does-not-work\/152940/);
  assert.match(text, /nymira_ask/);
  assert.match(text, /nymirad agent show/);
  assert.match(text, /different OS user|dedicated `nymirad` instance/);
  assert.match(text, /not tested|not run|has not been/i, "no Cursor version is claimed");
});

test("IronClaw has no MCP entry because v1.4.1 rejects stdio, and both READMEs say so", () => {
  assert.throws(() => hostConfig({ host: "ironclaw" as never, installDir: "/opt/x", tokenFile: "/t", transport: "tcp" }), /host/);
  assert.match(section("IronClaw"), /stdio/);
  assert.match(mcpReadme, /\| IronClaw \|[^\n]*stdio/);
  assert.doesNotMatch(mcpReadme, /configuration format was not checked/);
});
