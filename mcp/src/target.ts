// Where nymirad is: loopback TCP or a Unix socket, nothing else.
import { StartupError } from "./token.ts";

export type Transport = "tcp" | "unix";

export type Target =
  | { transport: "tcp"; host: string; port: number }
  | { transport: "unix"; socketPath: string };

const DEFAULT_PORT = 8788;

function loopbackHost(hostname: string): string | null {
  const h = hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (h === "localhost") return "127.0.0.1";
  if (h === "::1") return "::1";
  const m = /^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(h);
  if (m && m.slice(1).every((p) => Number(p) <= 255)) return h;
  return null;
}

export function resolveTarget(opts: {
  transport: string | undefined;
  url: string | undefined;
  socket: string | undefined;
}): Target {
  if (opts.transport !== "tcp" && opts.transport !== "unix") {
    throw new StartupError("--transport must be tcp or unix (the transport the token was made for)");
  }
  if (opts.transport === "unix") {
    if (opts.url) throw new StartupError("--url is for the tcp transport; use --socket");
    if (!opts.socket || !opts.socket.startsWith("/")) {
      throw new StartupError("--socket needs the absolute path of the agent socket");
    }
    return { transport: "unix", socketPath: opts.socket };
  }
  if (opts.socket) throw new StartupError("--socket is for the unix transport; use --url");
  let url: URL;
  try {
    url = new URL(opts.url ?? `http://127.0.0.1:${DEFAULT_PORT}`);
  } catch {
    throw new StartupError("--url is not a URL");
  }
  if (url.protocol !== "http:") throw new StartupError("--url must be http on a loopback address");
  if (url.username || url.password || url.search || url.hash || url.pathname !== "/") {
    throw new StartupError("--url is a loopback address and a port, nothing more");
  }
  const host = loopbackHost(url.hostname);
  if (!host) throw new StartupError("--url must be a loopback address (127.x.x.x, ::1 or localhost)");
  const port = url.port ? Number(url.port) : 80;
  return { transport: "tcp", host, port };
}
