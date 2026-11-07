// Reads the agent token from a private file and nowhere else.
import { closeSync, constants, fstatSync, openSync, readSync } from "node:fs";

export class StartupError extends Error {}

const AGENT_PREFIX = "nyma_";
const OWNER_PREFIX = "nymo_";
const MAX_FILE = 4096;
const SHAPE = /^nyma_[0-9a-f]{64}$/;

/**
 * The file must be a regular file opened without following a link, with no world access and no
 * group write or execute: mode 0600, or 0640 for the group an owner gave a socket agent. No
 * message here contains the file's content.
 */
export function readTokenFile(path: string): string {
  let fd: number;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  } catch (e) {
    const code = (e as NodeJS.ErrnoException).code;
    if (code === "ELOOP") throw new StartupError(`token file ${path} is a symbolic link`);
    throw new StartupError(`token file ${path} cannot be opened (${code ?? "error"})`);
  }
  try {
    const st = fstatSync(fd);
    if (!st.isFile()) throw new StartupError(`token file ${path} is not a regular file`);
    const mode = st.mode & 0o777;
    if (mode & 0o007) throw new StartupError(`token file ${path} is readable by other users`);
    if (mode & 0o137) {
      // Group write or execute, or an owner execute bit, is never part of a token file.
      throw new StartupError(`token file ${path} has mode ${mode.toString(8)}; use 0600 or 0640`);
    }
    if (st.size > MAX_FILE) throw new StartupError(`token file ${path} is too large`);
    const buf = Buffer.alloc(st.size);
    let off = 0;
    while (off < buf.length) {
      const n = readSync(fd, buf, off, buf.length - off, off);
      if (n === 0) break;
      off += n;
    }
    const text = buf.subarray(0, off).toString("utf8").replace(/\r?\n$/, "");
    if (text.startsWith(OWNER_PREFIX)) {
      throw new StartupError("the file holds an owner token; this server takes agent tokens only");
    }
    if (!text.startsWith(AGENT_PREFIX) || !SHAPE.test(text)) {
      throw new StartupError(`token file ${path} does not hold an agent token (nyma_...)`);
    }
    return text;
  } finally {
    closeSync(fd);
  }
}
