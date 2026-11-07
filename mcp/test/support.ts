// Helpers for the tests. They start the built server as a child process and talk to it with the
// official MCP client over stdio; no module of the server is imported.
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const here = dirname(fileURLToPath(import.meta.url));
export const ENTRY = join(here, "..", "dist", "index.js");
export const REPO = join(here, "..", "..");

export const AGENT_TOKEN = `nyma_${"ab".repeat(32)}`;
export const OWNER_TOKEN = `nymo_${"cd".repeat(32)}`;

export function scratch(): string {
  return mkdtempSync(join(tmpdir(), "nymira-mcp-test-"));
}

/** A token file with the given content and mode, in a fresh private directory. */
export function tokenFile(content: string, mode = 0o600): string {
  const path = join(scratch(), "agent.token");
  writeFileSync(path, content);
  chmodSync(path, mode);
  return path;
}

export interface Session {
  client: Client;
  transport: StdioClientTransport;
  stderr: () => string;
  pid: number;
  close: () => Promise<void>;
}

/** Connects the official client to the built server. */
export async function connect(args: string[], env: Record<string, string> = {}): Promise<Session> {
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [ENTRY, ...args],
    env: { PATH: process.env.PATH ?? "", ...env },
    stderr: "pipe",
  });
  let err = "";
  transport.stderr?.on("data", (c: Buffer) => (err += c.toString()));
  const client = new Client({ name: "nymira-mcp-test", version: "0.0.0" });
  await client.connect(transport);
  return {
    client,
    transport,
    stderr: () => err,
    pid: transport.pid as number,
    close: () => client.close(),
  };
}

/** Runs the server to the end; for startup refusals. */
export function runToExit(args: string[], env: Record<string, string> = {}) {
  const r = spawnSync(process.execPath, [ENTRY, ...args], {
    env: { PATH: process.env.PATH ?? "", ...env },
    input: "",
    encoding: "utf8",
    timeout: 20_000,
  });
  return { status: r.status, stdout: r.stdout, stderr: r.stderr };
}

export function tcpArgs(file: string, port: number): string[] {
  return ["--token-file", file, "--transport", "tcp", "--url", `http://127.0.0.1:${port}`];
}

/** A stand-in for nymirad that records every request and answers with `reply`. */
export interface Stub {
  port: number;
  requests: http.IncomingHttpHeaders[];
  close: () => Promise<void>;
}
export async function stub(reply: (url: string) => unknown = () => ({}), status = 200): Promise<Stub> {
  const requests: http.IncomingHttpHeaders[] = [];
  const server = http.createServer((req, res) => {
    requests.push({ ...req.headers, ":url": req.url } as http.IncomingHttpHeaders);
    req.resume();
    res.statusCode = status;
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify(reply(req.url ?? "")));
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  return {
    port: (server.address() as net.AddressInfo).port,
    requests,
    close: () => new Promise<void>((r) => server.close(() => r())),
  };
}

/** A TCP listener that only counts connections. */
export async function counter(): Promise<{ port: number; count: () => number; close: () => Promise<void> }> {
  let n = 0;
  const server = net.createServer((s) => {
    n++;
    s.destroy();
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  return {
    port: (server.address() as net.AddressInfo).port,
    count: () => n,
    close: () => new Promise<void>((r) => server.close(() => r())),
  };
}

// ---- the mock stack ----

export interface Fixture {
  info: {
    port: number;
    socket: string;
    model: string;
    token_files: { main: string; tight: string; sock: string };
    owner_token_file: string;
  };
  report: () => Promise<{ leases: number; chats: number; outbound: number }>;
  stop: () => Promise<void>;
}

/** Starts the Rust fixture: nymirad's real agent routes over the sandbox stack. */
export async function startFixture(): Promise<Fixture> {
  const bin = process.env.NYMIRA_MCP_FIXTURE ?? join(REPO, "target", "debug", "nymira-mcp-fixture");
  if (!existsSync(bin)) {
    throw new Error(`build the fixture first: cargo build -p nymira-mcp-fixture (looked for ${bin})`);
  }
  const child: ChildProcess = spawn(bin, [], { stdio: ["pipe", "pipe", "inherit"], cwd: REPO });
  let buf = "";
  const waiting: ((line: string) => void)[] = [];
  const lines: string[] = [];
  child.stdout?.on("data", (c: Buffer) => {
    buf += c.toString();
    let i: number;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      if (!line.startsWith("{")) continue;
      const w = waiting.shift();
      if (w) w(line);
      else lines.push(line);
    }
  });
  const next = () =>
    new Promise<string>((resolve, reject) => {
      const line = lines.shift();
      if (line !== undefined) return resolve(line);
      const t = setTimeout(() => reject(new Error("fixture did not answer")), 240_000);
      waiting.push((l) => {
        clearTimeout(t);
        resolve(l);
      });
    });
  const info = JSON.parse(await next());
  return {
    info,
    report: async () => {
      child.stdin?.write("report\n");
      return JSON.parse(await next());
    },
    // The fixture exits when stdin closes and takes the sandbox down with it.
    stop: () =>
      new Promise<void>((resolve) => {
        if (child.exitCode !== null) return resolve();
        const t = setTimeout(() => child.kill("SIGKILL"), 20_000);
        child.once("exit", () => {
          clearTimeout(t);
          resolve();
        });
        child.stdin?.end();
      }),
  };
}

/** `lsof` output for a process; lsof exits 1 when it finds nothing. */
export function lsof(pid: number, ...selectors: string[]): string {
  const r = spawnSync("lsof", ["-nP", "-a", "-p", String(pid), ...selectors, "-F", "pnTt"], { encoding: "utf8" });
  if (r.error) throw new Error("lsof is needed to list sockets");
  return r.stdout;
}
