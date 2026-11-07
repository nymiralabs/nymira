// Writes the MCP host entry that runs the fixed install. Not part of the package: it is run from a
// checkout (node tools/host-config.ts --host cursor ...) and prints JSON to paste into the host's file.
// The token never goes into the entry; only the path of its file does.
import { parseArgs } from "node:util";
import { resolveTarget } from "../src/target.ts";

export const HOSTS = ["cursor", "claude-desktop", "vscode"] as const;
export type Host = (typeof HOSTS)[number];

export interface Options {
  host: Host;
  /** The fixed install: the directory given to `npm install --prefix`. */
  installDir: string;
  tokenFile: string;
  transport: "tcp" | "unix";
  url?: string;
  socket?: string;
  /** The node that runs the server; hosts started from a menu or dock have a short PATH. */
  node?: string;
}

const absolute = (what: string, p: string) => {
  if (!p.startsWith("/")) throw new Error(`${what} must be an absolute path`);
  return p;
};

export function hostConfig(o: Options): Record<string, unknown> {
  if (!HOSTS.includes(o.host)) throw new Error(`host must be one of ${HOSTS.join(", ")}`);
  absolute("install directory", o.installDir);
  absolute("token file", o.tokenFile);
  const target = resolveTarget({ transport: o.transport, url: o.url, socket: o.socket }); // same refusals as the server
  const args = [
    `${o.installDir.replace(/\/$/, "")}/node_modules/@nymiralabs/nymira-mcp/dist/index.js`,
    "--token-file", o.tokenFile,
    ...(target.transport === "unix"
      ? ["--transport", "unix", "--socket", target.socketPath]
      : ["--transport", "tcp", "--url", o.url ?? "http://127.0.0.1:8788"]),
  ];
  const entry = { command: absolute("node", o.node ?? process.execPath), args };
  if (o.host === "vscode") return { servers: { nymira: { type: "stdio", ...entry } } };
  // Cursor's documentation lists `type` as required for a local server.
  if (o.host === "cursor") return { mcpServers: { nymira: { type: "stdio", ...entry } } };
  return { mcpServers: { nymira: entry } };
}

if (import.meta.main) {
  const { values: v } = parseArgs({
    options: {
      host: { type: "string" }, "install-dir": { type: "string" }, "token-file": { type: "string" },
      transport: { type: "string" }, url: { type: "string" }, socket: { type: "string" }, node: { type: "string" },
    },
    strict: true,
  });
  try {
    process.stdout.write(`${JSON.stringify(hostConfig({
      host: v.host as Host, installDir: v["install-dir"] ?? "", tokenFile: v["token-file"] ?? "",
      transport: v.transport as "tcp" | "unix", url: v.url, socket: v.socket, node: v.node,
    }), null, 2)}\n`);
  } catch (e) {
    process.stderr.write(`host-config: ${(e as Error).message}\n`);
    process.exit(1);
  }
}
