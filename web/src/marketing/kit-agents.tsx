import claudeLogo from './assets/brands/agent-claude.svg';
import codexLogo from './assets/brands/agent-codex.svg';
import cursorLogo from './assets/brands/agent-cursor.svg';
import ironclawLogo from './assets/brands/agent-ironclaw.png';
import vscodeLogo from './assets/brands/agent-vscode.png';
import { vars } from './kit';

// The client list and its picker, shared by the console preview and the real Agents screen.
export type ClientId = 'claude' | 'cursor' | 'vscode' | 'codex' | 'ironclaw';
export interface ClientInfo {
  id: ClientId;
  name: string;
  logo: string;
  via: 'MCP over stdio' | 'OpenAI-compatible API';
  status: string;
  host?: string;
  file?: string;
  transport: 'tcp' | 'unix';
}
export const CLIENTS: readonly ClientInfo[] = [
  {
    id: 'claude',
    name: 'Claude Desktop',
    logo: claudeLogo,
    via: 'MCP over stdio',
    status: 'entry generated',
    host: 'claude-desktop',
    file: 'claude_desktop_config.json',
    transport: 'unix',
  },
  {
    id: 'cursor',
    name: 'Cursor',
    logo: cursorLogo,
    via: 'MCP over stdio',
    status: 'entry generated, host not run',
    host: 'cursor',
    file: '~/.cursor/mcp.json',
    transport: 'unix',
  },
  {
    id: 'vscode',
    name: 'VS Code',
    logo: vscodeLogo,
    via: 'MCP over stdio',
    status: 'entry generated',
    host: 'vscode',
    file: '.vscode/mcp.json',
    transport: 'unix',
  },
  {
    id: 'ironclaw',
    name: 'IronClaw',
    logo: ironclawLogo,
    via: 'OpenAI-compatible API',
    status: 'recipe below',
    transport: 'tcp',
  },
  {
    id: 'codex',
    name: 'Codex',
    logo: codexLogo,
    via: 'OpenAI-compatible API',
    status: 'not documented here',
    transport: 'tcp',
  },
];

export function ClientMark({ client, size = 28 }: { client: ClientInfo; size?: number }) {
  return (
    <span className="nc-client-mark" style={vars({ '--s': `${size}px` })}>
      <img src={client.logo} alt="" width={size} height={size} draggable={false} />
    </span>
  );
}

export function ClientGrid({
  pick,
  onPick,
}: {
  pick: ClientId | null;
  onPick: (c: ClientInfo) => void;
}) {
  return (
    <div className="nc-clients" role="radiogroup" aria-label="Client">
      {CLIENTS.map((c) => (
        <button
          key={c.id}
          type="button"
          role="radio"
          aria-checked={pick === c.id}
          title={`${c.via}: ${c.status}`}
          onClick={() => onPick(c)}
        >
          <ClientMark client={c} size={36} />
          <strong>{c.name}</strong>
          <small>{c.via}</small>
          <em>{c.status}</em>
        </button>
      ))}
    </div>
  );
}

const num = (v: string) => (/^[0-9]+(\.[0-9]+)?$/.test(v.trim()) ? Number(v) : NaN);

/** The limits as bars on one scale, so the order (day, lease, ask, call) is visible while typing. */
export function BudgetLadder({
  rows,
}: {
  rows: readonly { key: string; label: string; value: string }[];
}) {
  const nums = rows.map((r) => num(r.value));
  const top = Math.max(0, ...nums.filter((n) => Number.isFinite(n)));
  return (
    <div className="nc-ladder" aria-hidden="true">
      {rows.map((r, i) => {
        const n = nums[i] ?? NaN;
        const set = Number.isFinite(n) && n > 0 && top > 0;
        return (
          <div className="nc-ladder-row" key={r.key} data-set={set}>
            <span>{r.label}</span>
            <i>
              <b style={vars({ '--w': set ? `${Math.max(3, (n / top) * 100)}%` : '0%' })} />
            </i>
            <em>{set ? `$${r.value.trim()}` : 'not set'}</em>
          </div>
        );
      })}
    </div>
  );
}
