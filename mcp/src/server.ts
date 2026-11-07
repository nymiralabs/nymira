import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import type { Fetcher } from "./nymirad.ts";
import { runTool, TOOLS } from "./tools.ts";

/** Only the tools capability: no list-changed notices, prompts, resources, sampling or logging. */
export function buildServer(fetcher: Fetcher, version: string): Server {
  const server = new Server({ name: "nymira-mcp", version }, { capabilities: { tools: {} } });
  server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: TOOLS.map((t) => structuredClone(t)) }));
  server.setRequestHandler(CallToolRequestSchema, async (req) =>
    runTool(fetcher, req.params.name, req.params.arguments),
  );
  return server;
}
