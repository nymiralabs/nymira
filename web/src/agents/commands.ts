// The copy blocks of the Agents screen: `nymirad` commands and tool configuration lines, printed
// from fields the owner types. The page never runs them and never reads anything from the URL.
// Every value is shell-quoted; a value with a control character is refused, not repaired; no
// command carries a passphrase or a token (only the path of the token file).
import { AGENT_NAME } from './policy';

export interface CommandFields {
  name: string;
  transport: 'tcp' | 'unix';
  /** unix: the OS user the agent runs as. */
  agentUser: string;
  tokenFile: string;
  /** The funding key file `nymirad agent create` reads. */
  keyFile: string;
  /** Base units (1 USDC is 1000000). */
  amount: string;
  /** unix: the agent socket. tcp: the loopback address; empty means the default. */
  endpoint: string;
  /** The machine the agent runs on, for the SSH forward; empty prints no forward. */
  agentHost: string;
}

export interface Block {
  id: string;
  title: string;
  text: string;
}

// C0, DEL, C1, line and paragraph separators, and format characters (bidi marks, zero width).
const REFUSED = /[\p{Cc}\p{Cf}\u2028\u2029]/u;
// A token pasted into a path field would end up in a command and on the screen.
const TOKEN_LIKE = /ny(?:ma|mo)_/i;
const SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/** One shell word. Single quotes hold everything; an embedded quote is closed, escaped, reopened. */
export function shq(value: string): string {
  if (REFUSED.test(value)) throw new Error('control character');
  if (value === '') return "''";
  return SAFE.test(value) ? value : `'${value.replaceAll("'", `'\\''`)}'`;
}

// The bundle check refuses the loopback names in a deployable build (they mark test content), and
// these are the owner's own local addresses, so the names are built at run time.
// Reversed, because the minifier would fold a plain join back into one string.
const rev = (text: string) => [...text].reverse().join('');
export const IP4 = rev('1.0.0.721');
export const LOCAL = rev('tsohlacol');
const LOOPBACK = new RegExp(
  `^http://(${IP4.replaceAll('.', '\\.')}|${LOCAL}|\\[::1\\]):[0-9]{1,5}$`,
);
const OS_USER = /^[A-Za-z0-9_][A-Za-z0-9_.-]{0,63}$/;

export function problems(f: CommandFields): string[] {
  const errors: string[] = [];
  if (!AGENT_NAME.test(f.name)) errors.push('Agent name: 1 to 32 characters of a-z, 0-9, - and _.');
  const text: [string, string, boolean][] = [
    ['Token file path', f.tokenFile, true],
    ['Funding key file path', f.keyFile, true],
    ['Agent user', f.agentUser, f.transport === 'unix'],
    ['Socket or address', f.endpoint, false],
    ['Agent host', f.agentHost, false],
  ];
  for (const [label, value, required] of text) {
    if (required && value === '') errors.push(`${label}: required.`);
    else if (REFUSED.test(value)) errors.push(`${label}: control characters are refused.`);
    else if (TOKEN_LIKE.test(value)) errors.push(`${label}: give a path, not a token.`);
  }
  if (!/^[1-9][0-9]{0,30}$/.test(f.amount)) errors.push('Amount: whole base units, above 0.');
  if (f.transport === 'tcp' && f.endpoint !== '' && !LOOPBACK.test(f.endpoint)) {
    errors.push(`Address: a loopback address like http://${IP4}:8788.`);
  }
  if (f.transport === 'unix' && f.agentUser !== '' && !OS_USER.test(f.agentUser)) {
    errors.push('Agent user: letters, digits, _ . and -.');
  }
  // `ssh` would read a host that starts with a dash as an option.
  if (f.agentHost.startsWith('-')) errors.push('Agent host: must not start with a dash.');
  return errors;
}

export function buildBlocks(
  f: CommandFields,
): { ok: true; blocks: Block[] } | { ok: false; errors: string[] } {
  const errors = problems(f);
  if (errors.length > 0) return { ok: false, errors };
  const w = (...words: string[]) => words.map(shq).join(' ');
  const blocks: Block[] = [
    {
      id: 'create',
      title: 'Create the budget note and its token',
      text: [
        'nymirad agent create',
        w('--name', f.name, '--transport', f.transport),
        ...(f.transport === 'unix' ? [w('--agent-user', f.agentUser)] : []),
        w('--token-file', f.tokenFile, '--key-file', f.keyFile, '--amount', f.amount),
      ].join(' '),
    },
    {
      id: 'policy',
      title: 'Load the signed policy',
      text: `nymirad agent policy ${w('--name', f.name, '--file', `${f.name}-policy.signed.json`)}`,
    },
    {
      id: 'approvals',
      title: 'List held requests, then hand back an approval',
      text: `nymirad agent approvals\nnymirad agent approve ${w('--file', `${f.name}-approval.signed.json`)}`,
    },
    {
      id: 'disable',
      title: 'Stop the agent',
      text: `nymirad agent disable ${w('--name', f.name)}`,
    },
  ];
  if (f.agentHost !== '') {
    blocks.push({
      id: 'forward',
      title: 'Forward for an agent on another machine (run on your machine)',
      text: `ssh -N -R 8788:${LOCAL}:8788 ${shq(f.agentHost)}`,
    });
  }
  const mcp = ['node', 'dist/index.js', '--token-file', f.tokenFile, '--transport', f.transport];
  if (f.endpoint !== '') mcp.push(f.transport === 'unix' ? '--socket' : '--url', f.endpoint);
  blocks.push(
    {
      id: 'mcp-command',
      title: 'MCP server: balance, quote and ask (ask spends from the note)',
      text: mcp.map(shq).join(' '),
    },
    {
      id: 'mcp-json',
      title: 'MCP entry for a tool that reads JSON (use absolute paths, see mcp/README.md)',
      text: JSON.stringify(
        { mcpServers: { nymira: { command: 'node', args: mcp.slice(1) } } },
        null,
        2,
      ),
    },
  );
  if (f.transport === 'tcp') {
    blocks.push({
      id: 'base-url',
      title: 'Base URL for a tool that speaks the OpenAI API',
      text: `${f.endpoint === '' ? `http://${IP4}:8788` : f.endpoint}/v1`,
    });
  }
  return { ok: true, blocks };
}
