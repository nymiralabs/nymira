import { execFileSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { agentsAvailable } from '../src/agents/available';
import { buildBlocks, shq, type CommandFields } from '../src/agents/commands';
import {
  approvalMessage,
  approvalRecipient,
  buildPolicyYaml,
  checkAgent,
  policyMessage,
  policyRecipient,
  signedFile,
  type PolicyFields,
} from '../src/agents/policy';

const GOOD: PolicyFields = {
  models: ['z-ai/glm-5.3-flash', 'Qwen/Qwen3.8-27B'],
  maxLeaseUsd: '0.5',
  maxCallUsd: '0.05',
  maxDailyUsd: '2',
  confirmAboveUsd: '0.03',
  rateLimitPerMin: '30',
};

describe('policy.yaml', () => {
  it('is exactly the subset nymirad reads, with require_tee true', () => {
    const built = buildPolicyYaml(GOOD);
    expect(built).toEqual({
      ok: true,
      value:
        'allowed_models:\n  - z-ai/glm-5.3-flash\n  - Qwen/Qwen3.8-27B\n' +
        'max_lease_usd: 0.5\nmax_call_usd: 0.05\nmax_daily_usd: 2\nconfirm_above_usd: 0.03\n' +
        'rate_limit_per_min: 30\nrequire_tee: true\n',
    });
  });

  it.each([
    ['no model', { models: [] }],
    ['an unpinned model', { models: ['gpt-9'] }],
    ['a model twice', { models: ['Qwen/Qwen3.8-27B', 'Qwen/Qwen3.8-27B'] }],
    ['a lease below the smallest class', { maxLeaseUsd: '0.05' }],
    ['zero', { maxCallUsd: '0' }],
    ['a negative number', { maxDailyUsd: '-1' }],
    ['an exponent', { maxDailyUsd: '1e3' }],
    ['a trailing dot', { confirmAboveUsd: '1.' }],
    ['two dots', { confirmAboveUsd: '1.2.3' }],
    ['more than 12 characters', { maxDailyUsd: '1.2345678901234' }],
    ['a YAML injection', { maxCallUsd: '1\nallowed_origins: [x]' }],
    ['rate zero', { rateLimitPerMin: '0' }],
    ['a fractional rate', { rateLimitPerMin: '1.5' }],
    ['a rate above u32', { rateLimitPerMin: '4294967296' }],
  ])('refuses %s', (_label, change) => {
    expect(buildPolicyYaml({ ...GOOD, ...change }).ok).toBe(false);
  });
});

describe('signed messages', () => {
  it('builds the policy message and the recipients nymirad expects', () => {
    const yaml = 'a: b\n';
    expect(policyMessage(3, yaml)).toEqual({
      ok: true,
      value: '{"kind":"nymira-policy","version":3,"policy":"a: b\\n"}',
    });
    expect(policyMessage(0, yaml).ok).toBe(false);
    expect(policyMessage(1.5, yaml).ok).toBe(false);
    expect(policyMessage(2 ** 32, yaml).ok).toBe(false);
    expect(policyRecipient('cursor', '0xabc')).toBe('nymirad:cursor:0xabc');
    expect(approvalRecipient('cursor', '0xabc')).toBe('nymirad-approval:cursor:0xabc');
  });

  const approval = {
    id: 'apv_' + 'a'.repeat(32),
    requestHash: 'b'.repeat(64),
    amountMicro: '42000',
    nonce: 'c'.repeat(64),
  };

  it('builds the approval message with the fields nymirad checks', () => {
    expect(approvalMessage(approval)).toEqual({
      ok: true,
      value: JSON.stringify({
        kind: 'nymira-approval',
        id: approval.id,
        request_hash: approval.requestHash,
        amount_micro: '42000',
        nonce: approval.nonce,
      }),
    });
  });

  it.each([
    ['id', { id: 'apv_zz' }],
    ['id with a quote', { id: 'apv_' + 'a'.repeat(31) + '"' }],
    ['hash in capitals', { requestHash: 'B'.repeat(64) }],
    ['a short hash', { requestHash: 'b'.repeat(63) }],
    ['a decimal amount', { amountMicro: '0.5' }],
    ['a leading zero', { amountMicro: '042' }],
    ['zero', { amountMicro: '0' }],
    ['a short nonce', { nonce: 'c'.repeat(8) }],
  ])('refuses an approval with a bad %s', (_label, change) => {
    expect(approvalMessage({ ...approval, ...change }).ok).toBe(false);
  });

  it('checks the agent name and the commitment', () => {
    expect(checkAgent('cursor', '0x1a2b')).toEqual([]);
    expect(checkAgent('Cursor', '0x1a2b')).toHaveLength(1);
    expect(checkAgent('a:b', '0x1a2b')).toHaveLength(1);
    expect(checkAgent('cursor', '0x1a2b:evil')).toHaveLength(1);
  });

  it('writes the signed file with the six fields and a base64 nonce', () => {
    const file = JSON.parse(
      signedFile({
        accountId: 'owner.testnet',
        publicKey: 'ed25519:xyz',
        signature: 'c2ln',
        nonce: new Uint8Array(32).fill(9),
        message: '{"m":1}',
        recipient: 'nymirad:a:0x1',
      }),
    ) as Record<string, string>;
    expect(Object.keys(file)).toEqual([
      'account_id',
      'public_key',
      'signature',
      'nonce',
      'message',
      'recipient',
    ]);
    expect(Buffer.from(file.nonce!, 'base64')).toHaveLength(32);
  });
});

const FIELDS: CommandFields = {
  name: 'cursor',
  transport: 'tcp',
  agentUser: '',
  tokenFile: '/srv/agent/cursor.token',
  keyFile: '/home/me/key.json',
  amount: '1000000',
  endpoint: '',
  agentHost: 'agent.example',
};

const blocksOf = (f: CommandFields) => {
  const built = buildBlocks(f);
  if (!built.ok) throw new Error(built.errors.join('; '));
  return Object.fromEntries(built.blocks.map((b) => [b.id, b.text]));
};

// Runs the quoted word through a real shell and reads back what it received.
const shellWord = (value: string) =>
  execFileSync('/bin/sh', ['-c', `printf %s ${shq(value)}`]).toString('utf8');

describe('copy blocks', () => {
  it('prints the commands the owner needs, with the fixed ssh forward', () => {
    const b = blocksOf(FIELDS);
    expect(b.create).toBe(
      'nymirad agent create --name cursor --transport tcp --token-file /srv/agent/cursor.token --key-file /home/me/key.json --amount 1000000',
    );
    expect(b.forward).toBe('ssh -N -R 8788:localhost:8788 agent.example');
    expect(b.policy).toBe('nymirad agent policy --name cursor --file cursor-policy.signed.json');
    expect(b['mcp-command']).toBe(
      'node dist/index.js --token-file /srv/agent/cursor.token --transport tcp',
    );
    expect(b['base-url']).toBe('http://127.0.0.1:8788/v1');
    expect(blocksOf({ ...FIELDS, agentHost: '' }).forward).toBeUndefined();
  });

  it('uses the unix flags for a socket agent', () => {
    const b = blocksOf({
      ...FIELDS,
      transport: 'unix',
      agentUser: 'agent1',
      endpoint: '/tmp/nymira-agent-501/agent.sock',
    });
    expect(b.create).toContain('--transport unix --agent-user agent1 ');
    expect(b['mcp-command']).toContain(
      '--transport unix --socket /tmp/nymira-agent-501/agent.sock',
    );
    expect(b['base-url']).toBeUndefined();
  });

  const HOSTILE = [
    "it's",
    "'; rm -rf ~ #",
    '$(id)',
    '`id`',
    '${HOME}',
    'a b',
    'a"b',
    'back\\slash',
    '*',
    '&& echo x',
    '~/x',
    'é ü',
  ];
  it.each(HOSTILE)('quotes %j so a shell reads the same word', (value) => {
    expect(shellWord(value)).toBe(value);
    const b = blocksOf({ ...FIELDS, tokenFile: value, keyFile: value, agentHost: value });
    // The whole command is one line, and the hostile text only ever sits inside a quoted word.
    for (const id of ['create', 'forward', 'mcp-command']) expect(b[id]).not.toContain('\n');
  });

  it('puts a hostile path in a command that parses back to the same words', () => {
    const value = 'x\'; $(touch /tmp/pwned) #"';
    const line = blocksOf({ ...FIELDS, tokenFile: value }).create!;
    const words = execFileSync('/bin/sh', [
      '-c',
      `set -- ${line}; for a; do printf '%s\\n' "$a"; done`,
    ])
      .toString('utf8')
      .split('\n');
    expect(words).toContain(value);
  });

  it.each([
    ['a newline', 'a\nb'],
    ['a carriage return', 'a\rb'],
    ['a NUL', 'a\0b'],
    ['an escape', 'a\u001bb'],
    ['DEL', 'a\u007fb'],
    ['a C1 control', 'a\u0085b'],
    ['a line separator', 'a b'],
    ['a right-to-left override', 'a‮b'],
  ])('refuses %s in every free field', (_label, value) => {
    for (const field of ['tokenFile', 'keyFile', 'endpoint', 'agentHost'] as const) {
      expect(buildBlocks({ ...FIELDS, [field]: value }).ok).toBe(false);
    }
    expect(() => shq(value)).toThrow();
    expect(buildBlocks({ ...FIELDS, transport: 'unix', agentUser: value }).ok).toBe(false);
  });

  it('refuses a hostile agent name, a dashed ssh host and a token in a path', () => {
    expect(buildBlocks({ ...FIELDS, name: "x'; id #" }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, name: 'Upper' }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, agentHost: '-oProxyCommand=id' }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, tokenFile: 'nyma_0123abcd' }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, tokenFile: '/x/nymo_owner' }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, amount: '1; id' }).ok).toBe(false);
    expect(buildBlocks({ ...FIELDS, endpoint: 'http://example.com:8788' }).ok).toBe(false);
  });

  it('never prints a passphrase flag or a token', () => {
    const all = Object.values(blocksOf(FIELDS)).join('\n');
    expect(all).not.toMatch(/passphrase|nyma_|nymo_|NYMIRA_/i);
  });
});

describe('who sees the screen', () => {
  const w = (id: string, signMessage: boolean) => ({ id, name: id, injected: false, signMessage });
  it('hides it for a connected wallet that cannot sign, or when none can', () => {
    expect(agentsAvailable([], null)).toBe(true);
    expect(agentsAvailable([w('a', true), w('b', false)], null)).toBe(true);
    expect(agentsAvailable([w('b', false)], null)).toBe(false);
    expect(agentsAvailable([w('a', true), w('b', false)], 'b')).toBe(false);
    expect(agentsAvailable([w('a', true), w('b', false)], 'a')).toBe(true);
    expect(agentsAvailable([w('a', true)], 'gone')).toBe(false);
  });
});
