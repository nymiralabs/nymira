import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { DEFAULTS, SOURCE, rewrite } from '../../scripts/recipes.mjs';
import { markdown } from '../../shared/recipes.mjs';
import type { CommandFields } from '../src/agents/commands';
import { IP4, LOCAL } from '../src/agents/commands';
import { recipeViews } from '../src/agents/recipes';

const ROOT = join(import.meta.dirname, '..', '..');
const readme = readFileSync(join(ROOT, 'BUDGET_NOTES.md'), 'utf8');

const FIELDS: CommandFields = {
  name: 'ironclaw',
  transport: 'tcp',
  agentUser: '',
  tokenFile: '/srv/agent/ironclaw.token',
  keyFile: '',
  amount: '',
  endpoint: '',
  agentHost: '',
};

function views(fields: Partial<CommandFields>) {
  const built = recipeViews({ ...FIELDS, ...fields });
  if (!built.ok) throw new Error(built.errors.join('; '));
  return built.views;
}

/** The README text between a recipe's markers. */
function readmeBody(id: string): string {
  const m = new RegExp(
    `<!-- recipe:${id} begin[^\\n]*-->\\n\\n([\\s\\S]*?)\\n\\n<!-- recipe:${id} end -->`,
  ).exec(readme);
  if (!m) throw new Error(`README has no section for ${id}`);
  return m[1]!;
}

describe('one source for the README and the Agents screen', () => {
  it('the README sections are what shared/recipes.json renders', () => {
    expect(rewrite(readme)).toBe(readme);
  });

  it('the screen, filled with the README example values, shows the README text', () => {
    const ironclaw = views({ endpoint: `http://${LOCAL}:8788`, agentHost: 'AGENT_HOST' }).find(
      (v) => v.id === 'ironclaw',
    )!;
    expect(markdown(ironclaw.blocks)).toBe(readmeBody('ironclaw'));
    const cursor = views({
      name: 'cursor',
      tokenFile: '/srv/agent/cursor.token',
      endpoint: `http://${IP4}:8788`,
    }).find((v) => v.id === 'cursor')!;
    expect(markdown(cursor.blocks)).toBe(readmeBody('cursor'));
  });

  it('the README examples are the defaults the generator holds', () => {
    expect(Object.keys(DEFAULTS).sort()).toEqual(
      SOURCE.recipes.map((r: { id: string }) => r.id).sort(),
    );
  });
});

describe('the screen fills the recipes from what the owner types', () => {
  it('puts the token file, address, agent user, host and name into the commands', () => {
    const out = views({
      name: 'worker',
      tokenFile: '/srv/my agent/worker.token',
      endpoint: `http://${IP4}:9000`,
      agentUser: 'bot',
      agentHost: 'agent.example',
    });
    const code = (id: string) =>
      out
        .find((v) => v.id === id)!
        .blocks.filter((b) => b.t === 'code')
        .map((b) => b.text)
        .join('\n');
    const ic = code('ironclaw');
    expect(ic).toContain(`cat '/srv/my agent/worker.token'`);
    expect(ic).toContain(`LLM_BASE_URL=http://${IP4}:9000/v1`);
    expect(ic).toContain('-o bot /dev/stdin ~bot/nymira.env');
    expect(ic).toContain(`ssh -N -R 8788:${LOCAL}:8788 agent.example`);
    const cursor = JSON.parse(code('cursor')) as {
      mcpServers: { nymira: { args: string[] } };
    };
    expect(cursor.mcpServers.nymira.args).toContain('/srv/my agent/worker.token');
    expect(cursor.mcpServers.nymira.args).toContain(`http://${IP4}:9000`);
    const prose = out
      .flatMap((v) => v.blocks)
      .map((b) => (b.t === 'p' ? b.text : ''))
      .join('\n');
    expect(prose).toContain('nymirad agent disable --name worker');
  });

  it('uses a socket for a unix token in the Cursor entry and says IronClaw needs tcp', () => {
    const out = views({ transport: 'unix', agentUser: 'bot', endpoint: '/run/nymira/agent.sock' });
    const cursor = out.find((v) => v.id === 'cursor')!;
    const entry = JSON.parse(cursor.blocks.find((b) => b.t === 'code')!.text) as {
      mcpServers: { nymira: { args: string[] } };
    };
    expect(entry.mcpServers.nymira.args.slice(-4)).toEqual([
      '--transport',
      'unix',
      '--socket',
      '/run/nymira/agent.sock',
    ]);
    const ironclaw = out.find((v) => v.id === 'ironclaw')!;
    expect(ironclaw.blocks.some((b) => b.t === 'code')).toBe(false);
    expect(JSON.stringify(ironclaw.blocks)).toContain('tcp token');
  });

  it('refuses a token or a control character in a field, as the command blocks do', () => {
    expect(recipeViews({ ...FIELDS, tokenFile: 'nyma_abcdef0123456789' }).ok).toBe(false);
    expect(recipeViews({ ...FIELDS, tokenFile: '/srv/a\nb' }).ok).toBe(false);
    expect(recipeViews({ ...FIELDS, endpoint: 'http://example.com:8788' }).ok).toBe(false);
    expect(recipeViews({ ...FIELDS, agentHost: '-oProxyCommand=x' }).ok).toBe(false);
    expect(recipeViews({ ...FIELDS, name: 'Bad Name' }).ok).toBe(false);
  });
});

// What no recipe may carry: a token, a public tunnel to nymirad, or Cursor's base URL setting.
// The patterns look at configuration keys and URLs in the code blocks, not at prose that explains
// why the override is not used.
const OWNER_OR_AGENT_TOKEN = /ny(?:ma|mo)_[A-Za-z0-9]{6,}/;
const TUNNEL =
  /ngrok|trycloudflare|cloudflared|localtunnel|loca\.lt|serveo|tailscale funnel|\.ts\.net|bore\.pub|\btunnel\b/i;
const CURSOR_BASE_URL_KEY =
  /openai[._ -]?base[._ -]?url|override[._ -]?openai|base[._ -]?url[._ -]?override|"baseUrl"|"base_url"|OPENAI_BASE_URL|OPENAI_API_BASE|api[._ -]?base/i;

function codeOf(set: ReturnType<typeof views>): { id: string; text: string }[] {
  return set.flatMap((v) =>
    v.blocks.filter((b) => b.t === 'code').map((b) => ({ id: v.id, text: b.text })),
  );
}

describe('what no recipe contains', () => {
  const filled = [
    views({}),
    views({ transport: 'unix', agentUser: 'bot', endpoint: '/run/nymira/agent.sock' }),
    views({ endpoint: `http://${IP4}:8788`, agentHost: 'host.example' }),
  ];

  it('no owner token or token value in any recipe, filled or not', () => {
    for (const set of filled)
      for (const c of codeOf(set)) expect(c.text).not.toMatch(OWNER_OR_AGENT_TOKEN);
    expect(JSON.stringify(SOURCE)).not.toMatch(OWNER_OR_AGENT_TOKEN);
    for (const id of ['ironclaw', 'cursor'])
      expect(readmeBody(id)).not.toMatch(OWNER_OR_AGENT_TOKEN);
  });

  it('every URL in a code block is a loopback address, and none names a tunnel', () => {
    for (const set of filled)
      for (const c of codeOf(set)) {
        expect(c.text).not.toMatch(TUNNEL);
        for (const url of c.text.match(/https?:\/\/[^\s'"/]+/g) ?? []) {
          expect(url).toMatch(
            new RegExp(`^http://(${IP4.replaceAll('.', '\\.')}|${LOCAL}|\\[::1\\])(:[0-9]{1,5})?$`),
          );
        }
      }
  });

  it('the Cursor recipe has no base URL configuration', () => {
    for (const set of filled)
      for (const c of codeOf(set).filter((x) => x.id === 'cursor')) {
        expect(c.text).not.toMatch(CURSOR_BASE_URL_KEY);
        expect(c.text).not.toMatch(/\/v1\b/);
      }
  });

  it('the checks themselves match what they are for', () => {
    expect('LLM_API_KEY=nyma_0123456789abcdef').toMatch(OWNER_OR_AGENT_TOKEN);
    expect('nymo_0123456789abcdef').toMatch(OWNER_OR_AGENT_TOKEN);
    expect('ssh -R 80:x ngrok.io').toMatch(TUNNEL);
    expect('"openai.baseUrl": "https://x/v1"').toMatch(CURSOR_BASE_URL_KEY);
    expect('OPENAI_BASE_URL=http://x/v1').toMatch(CURSOR_BASE_URL_KEY);
  });

  it('the source holds no loopback name, which the deployable bundle check refuses', () => {
    const text = JSON.stringify(SOURCE);
    expect(text).not.toContain(['local', 'host'].join(''));
    expect(text).not.toContain(['127', '0', '0', '1'].join('.'));
  });
});
