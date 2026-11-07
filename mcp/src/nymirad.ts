// The only network code: one HTTP/1.1 request to the owner's nymirad over loopback or a Unix
// socket. A fresh agent with no proxy support is used on purpose, so HTTP_PROXY and
// NODE_USE_ENV_PROXY change nothing.
import http from "node:http";
import type { Target } from "./target.ts";

const MAX_RESPONSE = 1 << 20;
const TIMEOUT_MS = 30_000;
// nymirad accepts these four hosts; the port it serves is fixed, whatever local port a forward uses.
const HOST_HEADER = "localhost:8788";

export class TransportError extends Error {
  readonly code: string;
  constructor(code: string) {
    super(code);
    this.code = code;
  }
}

export interface Reply {
  status: number;
  body: unknown;
  headers: http.IncomingHttpHeaders;
}

export type Fetcher = (
  method: "GET" | "POST",
  path: string,
  headers: Record<string, string>,
  body?: string,
) => Promise<Reply>;

export function makeFetcher(target: Target, token: string): Fetcher {
  const agent = new http.Agent({ keepAlive: false });
  return (method, path, headers, body) =>
    new Promise<Reply>((resolve, reject) => {
      const payload = body === undefined ? undefined : Buffer.from(body, "utf8");
      const req = http.request(
        {
          ...(target.transport === "unix"
            ? { socketPath: target.socketPath }
            : { host: target.host, port: target.port }),
          method,
          path,
          agent,
          timeout: TIMEOUT_MS,
          headers: {
            ...headers,
            host: HOST_HEADER,
            authorization: `Bearer ${token}`,
            accept: "application/json",
            ...(payload
              ? { "content-type": "application/json", "content-length": String(payload.length) }
              : {}),
          },
        },
        (res) => {
          const chunks: Buffer[] = [];
          let size = 0;
          res.on("data", (c: Buffer) => {
            size += c.length;
            if (size > MAX_RESPONSE) {
              req.destroy();
              reject(new TransportError("response_too_large"));
              return;
            }
            chunks.push(c);
          });
          res.on("end", () => {
            try {
              resolve({
                status: res.statusCode ?? 0,
                body: JSON.parse(Buffer.concat(chunks).toString("utf8")),
                headers: res.headers,
              });
            } catch {
              reject(new TransportError("response_not_json"));
            }
          });
          res.on("error", () => reject(new TransportError("connection_lost")));
        },
      );
      req.on("timeout", () => {
        req.destroy();
        reject(new TransportError("timeout"));
      });
      req.on("error", (e: NodeJS.ErrnoException) => {
        reject(new TransportError(e.code === "ECONNREFUSED" || e.code === "ENOENT" ? "nymirad_unreachable" : "connection_lost"));
      });
      if (payload) req.write(payload);
      req.end();
    });
}
