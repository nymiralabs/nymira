#!/usr/bin/env node
import { parseArgs } from "node:util";
import { readFileSync } from "node:fs";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { makeFetcher } from "./nymirad.ts";
import { buildServer } from "./server.ts";
import { resolveTarget } from "./target.ts";
import { readTokenFile, StartupError } from "./token.ts";

const USAGE = `nymira-mcp: MCP server (stdio) for one Nymira agent token

  --token-file PATH      file holding the agent token (mode 0600, or 0640 for a socket agent)
  --transport tcp|unix   the transport the token was made for
  --url URL              tcp: loopback address of nymirad (default http://127.0.0.1:8788)
  --socket PATH          unix: the agent socket

Each flag also reads NYMIRA_MCP_TOKEN_FILE, NYMIRA_MCP_TRANSPORT, NYMIRA_MCP_URL and
NYMIRA_MCP_SOCKET. The token itself is never taken from a flag or the environment.`;

async function main(): Promise<void> {
  const { values } = parseArgs({
    options: {
      "token-file": { type: "string" },
      transport: { type: "string" },
      url: { type: "string" },
      socket: { type: "string" },
      help: { type: "boolean" },
    },
    strict: true,
  });
  if (values.help) {
    process.stderr.write(`${USAGE}\n`);
    return;
  }
  const env = process.env;
  const tokenFile = values["token-file"] ?? env.NYMIRA_MCP_TOKEN_FILE;
  if (!tokenFile) throw new StartupError("--token-file is required");
  const target = resolveTarget({
    transport: values.transport ?? env.NYMIRA_MCP_TRANSPORT,
    url: values.url ?? env.NYMIRA_MCP_URL,
    socket: values.socket ?? env.NYMIRA_MCP_SOCKET,
  });
  const token = readTokenFile(tokenFile);
  const version = (JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version: string }).version;
  await buildServer(makeFetcher(target, token), version).connect(new StdioServerTransport());
}

main().catch((e: unknown) => {
  // Only our own messages are printed; they never hold the token.
  process.stderr.write(`nymira-mcp: ${e instanceof StartupError || (e instanceof TypeError && "code" in e) ? (e as Error).message : "failed to start"}\n`);
  process.exit(1);
});
