import { useState } from 'react';
import agentsIllustration from '../marketing/assets/app-agents.webp';
import { Button, Card, Chip, Fold, Screen } from '../marketing/kit';
import {
  BudgetLadder,
  ClientGrid,
  ClientMark,
  CLIENTS,
  type ClientId,
  type ClientInfo,
} from '../marketing/kit-agents';
import { buildBlocks, type CommandFields } from '../agents/commands';
import { recipeViews } from '../agents/recipes';
import { plainText } from '../../../shared/recipes.mjs';
import {
  approvalMessage,
  approvalRecipient,
  buildPolicyYaml,
  checkAgent,
  policyMessage,
  policyRecipient,
  signedFile,
  type Built,
} from '../agents/policy';
import { MODELS } from '../chat/models';
import { download } from './format';
import { useWallet } from './services';
import { WalletConnect } from './WalletConnect';
import './agents.css';

// This screen builds files and commands from what the owner types. It creates no budget note,
// asks for and shows no token, reads nothing from the URL and makes no request to `nymirad`:
// the only thing it calls is the owner's own wallet, to sign a message.

function Errors({ list, id }: { list: string[]; id: string }) {
  if (list.length === 0) return null;
  return (
    <ul className="nc-msg nc-list nc-todo" data-tone="err" data-testid={id}>
      {list.map((e) => (
        <li key={e}>{e}</li>
      ))}
    </ul>
  );
}

function Field({
  label,
  value,
  onChange,
  id,
  hint,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  id: string;
  hint?: string;
}) {
  return (
    <label className="nc-field">
      <span>{label}</span>
      <input
        type="text"
        data-testid={id}
        value={value}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
      {hint && <small className="nc-hint">{hint}</small>}
    </label>
  );
}

function Money({
  label,
  value,
  onChange,
  id,
  hint,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  id: string;
  hint?: string;
}) {
  return (
    <label className="nc-field ag-money">
      <span>{label}</span>
      <span className="ag-money-box">
        <b aria-hidden="true">$</b>
        <input
          type="text"
          inputMode="decimal"
          data-testid={id}
          value={value}
          autoComplete="off"
          spellCheck={false}
          placeholder="0.00"
          onChange={(e) => onChange(e.target.value)}
        />
        <small>USD</small>
      </span>
      {hint && <small className="nc-hint">{hint}</small>}
    </label>
  );
}

function CopyBlock({ id, title, text }: { id: string; title: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <li>
      <strong>{title}</strong>
      <pre className="nc-code" data-testid={`block-${id}`}>
        {text}
      </pre>
      <div className="nc-actions">
        <Button
          testId={`copy-${id}`}
          onClick={() => {
            navigator.clipboard
              .writeText(text)
              .then(() => setCopied(true))
              .catch(() => setCopied(false));
          }}
        >
          {copied ? 'Copied' : 'Copy'}
        </Button>
      </div>
    </li>
  );
}

/** One thing to sign: the exact message the wallet will show, the button, and the signed file. */
function SignPanel({
  id,
  what,
  message,
  recipient,
  fileName,
}: {
  id: string;
  what: string;
  message: Built<string>;
  recipient: string;
  fileName: string;
}) {
  const wallet = useWallet();
  const [file, setFile] = useState('');
  const [signature, setSignature] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const account = wallet.connected?.accountId;
  if (!message.ok) return <Errors list={message.errors} id={`${id}-errors`} />;
  return (
    <div className="nc-sign">
      <p className="nc-cap">
        You are about to sign this {what} as <strong>{account ?? 'your account'}</strong>, for the
        recipient <code data-testid={`${id}-recipient`}>{recipient}</code>:
      </p>
      <pre className="nc-code" data-testid={`${id}-message`}>
        {message.value}
      </pre>
      <div className="nc-actions">
        <Button
          primary
          testId={`${id}-sign`}
          disabled={!wallet.connected || busy}
          onClick={async () => {
            setBusy(true);
            setError('');
            setFile('');
            try {
              const signed = await wallet.signMessage(message.value, recipient);
              setSignature(signed.signature);
              setFile(signedFile(signed));
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy
            ? 'Waiting for the wallet…'
            : `Sign with ${wallet.connected ? 'your wallet' : 'a wallet (connect one first)'}`}
        </Button>
      </div>
      {error && (
        <p className="nc-msg" data-tone="err" data-testid={`${id}-error`}>
          {error}
        </p>
      )}
      {file && (
        <div className="nc-signed">
          <p className="nc-cap">
            Signature: <code data-testid={`${id}-signature`}>{signature}</code>
          </p>
          <pre className="nc-code" data-testid={`${id}-file`}>
            {file}
          </pre>
          <div className="nc-actions">
            <Button
              testId={`${id}-download`}
              onClick={() => download(file, fileName, 'application/json')}
            >
              Download {fileName}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

const HEX64 = /^0x[0-9a-fA-F]{64}$/;
const STEP_TITLES = ['Wallet', 'The agent', 'Policy'];

/** A fingerprint of the note commitment: one cell per byte, four levels of ink. */
function Fingerprint({ commitment }: { commitment: string }) {
  const ok = HEX64.test(commitment);
  return (
    <div className="ag-print" data-ok={ok} role="img" aria-label="Fingerprint of the commitment">
      {Array.from({ length: 32 }, (_, i) => {
        const byte = ok ? parseInt(commitment.slice(2 + i * 2, 4 + i * 2), 16) : 0;
        return <i key={i} data-l={ok ? 1 + Math.floor(byte / 65) : 0} />;
      })}
    </div>
  );
}

/** The side card: what this page has so far, as a permit that fills while you type. */
function Permit({
  name,
  commitment,
  models,
  version,
  rows,
  checks,
}: {
  name: string;
  commitment: string;
  models: string[];
  version: string;
  rows: { key: string; label: string; value: string }[];
  checks: boolean[];
}) {
  const next = checks.findIndex((c) => !c);
  const short =
    commitment.length > 18 ? `${commitment.slice(0, 10)}…${commitment.slice(-6)}` : commitment;
  return (
    <div className="ag-permit" data-testid="agent-permit">
      <div className="ag-permit-top">
        <span>Budget permit</span>
        <span>{version.trim() ? `policy v${version.trim()}` : 'no version'}</span>
      </div>
      <strong className="ag-permit-name" data-empty={!name}>
        {name || 'unnamed agent'}
      </strong>
      <code className="ag-permit-note" data-empty={!commitment}>
        {short || 'no note commitment'}
      </code>
      <Fingerprint commitment={commitment} />
      <div className="ag-perf" aria-hidden="true" />
      <div className="ag-permit-block">
        <small>Models</small>
        {models.length > 0 ? (
          <div className="ag-chips">
            {models.map((m) => (
              <span key={m}>{m}</span>
            ))}
          </div>
        ) : (
          <p>none picked</p>
        )}
      </div>
      <div className="ag-permit-block">
        <small>Limits</small>
        <BudgetLadder rows={rows} />
      </div>
      <div className="ag-perf" aria-hidden="true" />
      <ol className="ag-checks">
        {['Wallet connected', 'Agent and note named', 'Policy ready to sign'].map((t, i) => (
          <li key={t} data-done={checks[i]}>
            <i />
            {t}
          </li>
        ))}
      </ol>
      {next >= 0 && (
        <Button
          primary
          testId="agent-next"
          onClick={() =>
            document.getElementById(`ag-step-${next}`)?.scrollIntoView({ behavior: 'smooth' })
          }
        >
          Next: {STEP_TITLES[next]}
        </Button>
      )}
    </div>
  );
}

const EMPTY_POLICY = {
  models: [] as string[],
  maxLeaseUsd: '',
  maxCallUsd: '',
  maxDailyUsd: '',
  confirmAboveUsd: '',
  rateLimitPerMin: '',
};

export function Agents() {
  const wallet = useWallet();
  const [name, setName] = useState('');
  const [commitment, setCommitment] = useState('');
  const [policy, setPolicy] = useState(EMPTY_POLICY);
  const [version, setVersion] = useState('');
  const [approval, setApproval] = useState({ id: '', requestHash: '', amountMicro: '', nonce: '' });
  const [pick, setPick] = useState<ClientId | null>(null);
  const [cmd, setCmd] = useState<CommandFields>({
    name: '',
    transport: 'tcp',
    agentUser: '',
    tokenFile: '',
    keyFile: '',
    amount: '',
    endpoint: '',
    agentHost: '',
  });

  const agentErrors = checkAgent(name, commitment);
  const yaml = buildPolicyYaml(policy);
  const versionNumber = /^[0-9]{1,10}$/.test(version) ? Number(version) : NaN;
  const policyMsg: Built<string> =
    agentErrors.length > 0
      ? { ok: false, errors: agentErrors }
      : !yaml.ok
        ? yaml
        : policyMessage(versionNumber, yaml.value);
  const approvalMsg: Built<string> =
    agentErrors.length > 0 ? { ok: false, errors: agentErrors } : approvalMessage(approval);
  const blocks = buildBlocks({ ...cmd, name });
  const recipes = recipeViews({ ...cmd, name });
  const stepOf = (i: number) => {
    const done = [
      !!wallet.connected,
      agentErrors.length === 0,
      policyMsg.ok,
      false,
      blocks.ok,
      false,
    ];
    const firstOpen = done.findIndex((d, j) => !d && j < 3);
    return done[i] ? 'done' : i === (firstOpen < 0 ? 3 : firstOpen) ? 'now' : 'next';
  };
  const set = (k: keyof typeof policy) => (v: string) => setPolicy({ ...policy, [k]: v });
  const choose = (c: ClientInfo) => {
    setPick(c.id);
    setCmd({ ...cmd, transport: c.transport });
  };
  const picked = pick ? CLIENTS.find((c) => c.id === pick) : undefined;
  const views = recipes.ok
    ? [...recipes.views].sort((a, b) => Number(b.id === pick) - Number(a.id === pick))
    : [];

  return (
    <Screen
      testId="agents"
      title="Agents"
      lead={
        <span data-testid="agents-intro">
          A budget note is a note you fund for one agent. Its payout account is your own account,
          fixed at deposit, so whatever the agent leaves comes back to you. The agent gets a token
          and can spend that one note on inference. It cannot withdraw it anywhere but back to you.
        </span>
      }
      illustration={agentsIllustration}
      side={
        <Permit
          name={name}
          commitment={commitment}
          models={policy.models}
          version={version}
          rows={[
            { key: 'day', label: 'Per day', value: policy.maxDailyUsd },
            { key: 'lease', label: 'Per lease', value: policy.maxLeaseUsd },
            { key: 'ask', label: 'Ask above', value: policy.confirmAboveUsd },
            { key: 'call', label: 'Per call', value: policy.maxCallUsd },
          ]}
          checks={[!!wallet.connected, agentErrors.length === 0, policyMsg.ok]}
        />
      }
    >
      <p className="nc-notice" data-testid="agents-scope">
        This page creates no budget note, never asks for or shows a token, and makes no request to{' '}
        <code>nymirad</code>. It writes the policy file, signs it and your approvals with your
        wallet, and prints commands for you to run yourself.
      </p>
      <Fold title="What is guaranteed, and what is not" testId="agents-guarantees">
        <ul className="nc-list">
          <li>
            The ceiling is cryptographic. An agent can spend at most the balance of its budget note,
            and a close can only pay your payout account.
          </li>
          <li>
            The policy is not. The model list, the call, lease and daily limits, the rate limit, the
            approvals and <code>require_tee</code> are checked by <code>nymirad</code> on the
            machine that runs it. That machine is yours by default. If you move the note to the
            agent&apos;s machine (opt-in), it is the agent&apos;s, the policy is advisory there, and
            revoking is not guaranteed.
          </li>
          <li>
            Both rest on the vault matching the client&apos;s pins. The team can still update the
            vault code, and open user deposits in the vault are capped at 100 USDC (test USDC on
            testnet).
          </li>
          <li data-testid="agents-no-remote">
            There is no remote rate limit and no remote kill switch. A lease request does not name
            its note, and the design is meant to keep the operator from grouping leases by note.
            That is a claim of the underlying protocol that has not been checked independently. Only
            your own <code>nymirad</code> limits an agent&apos;s rate, and only you disable its
            token. The operator can still stop new leases for everyone.
          </li>
          <li data-testid="agents-close">
            Closing a budget note shows its note ID and total spend on chain, next to the payout
            account you named. The agent&apos;s total becomes public at the end.
          </li>
        </ul>
      </Fold>

      <ol className="nc-steps nc-agent-steps">
        <li id="ag-step-0" data-s={stepOf(0)}>
          <h2>Wallet</h2>
          <p className="nc-cap nc-cap-first">
            The policy and each approval are signed as NEP-413 messages with your NEAR wallet. The
            account must be the one that funded the budget note. <code>nymirad</code> checks that
            the signing key is a full-access key of that account when you load the policy.
          </p>
          <WalletConnect />
          {wallet.connected && wallet.connected.signature === 'unsupported' && (
            <p className="nc-cap">This browser cannot check the sign-in signature.</p>
          )}
        </li>

        <li id="ag-step-1" data-s={stepOf(1)}>
          <h2>The agent</h2>
          <Field label="Agent name" id="agent-name" value={name} onChange={setName} />
          <Field
            label="Budget note commitment"
            id="agent-commitment"
            value={commitment}
            onChange={setCommitment}
            hint="The note field of the agent in agents.json in the nymirad state directory."
          />
        </li>

        <li id="ag-step-2" data-s={stepOf(2)}>
          <h2>Policy</h2>
          <div className="ag-pol">
            <fieldset className="ag-group nc-models">
              <legend>Allowed models</legend>
              <div className="ag-models">
                {MODELS.map((m) => (
                  <label key={m.id} className="nc-check">
                    <input
                      type="checkbox"
                      data-testid={`model-${m.id}`}
                      checked={policy.models.includes(m.id)}
                      onChange={(e) =>
                        setPolicy({
                          ...policy,
                          models: e.target.checked
                            ? [...policy.models, m.id]
                            : policy.models.filter((x) => x !== m.id),
                        })
                      }
                    />
                    <span>{m.id}</span>
                  </label>
                ))}
              </div>
            </fieldset>
            <fieldset className="ag-group">
              <legend>Limits, from the widest to the narrowest</legend>
              <div className="ag-money-grid">
                <Money
                  label="Per day (UTC day)"
                  id="p-daily"
                  value={policy.maxDailyUsd}
                  onChange={set('maxDailyUsd')}
                />
                <Money
                  label="Per lease"
                  id="p-lease"
                  value={policy.maxLeaseUsd}
                  onChange={set('maxLeaseUsd')}
                  hint="At least 0.1."
                />
                <Money
                  label="Ask me above"
                  id="p-confirm"
                  hint="Per call."
                  value={policy.confirmAboveUsd}
                  onChange={set('confirmAboveUsd')}
                />
                <Money
                  label="Per call"
                  id="p-call"
                  value={policy.maxCallUsd}
                  onChange={set('maxCallUsd')}
                />
              </div>
            </fieldset>
            <fieldset className="ag-group">
              <legend>Rate and version</legend>
              <div className="ag-money-grid">
                <Field
                  label="Requests a minute"
                  id="p-rate"
                  value={policy.rateLimitPerMin}
                  onChange={set('rateLimitPerMin')}
                />
                <Field
                  label="Policy version"
                  id="p-version"
                  value={version}
                  onChange={setVersion}
                  hint="Must be higher than any version this agent has had."
                />
              </div>
            </fieldset>
            {!yaml.ok && <Errors list={yaml.errors} id="policy-needs" />}
            <Fold title="The policy file this signs" testId="policy-fold">
              {yaml.ok ? (
                <pre className="nc-code nc-yaml" data-testid="policy-yaml">
                  {yaml.value}
                </pre>
              ) : (
                <p className="nc-cap nc-cap-first">
                  It shows here when the fields above are complete.
                </p>
              )}
            </Fold>
            <p className="nc-cap" data-testid="p-tee">
              <code>require_tee: true</code> is the only value this page offers:{' '}
              <code>nymirad</code> releases an answer only after it checked the provider&apos;s
              signature on it.
            </p>
          </div>
          <SignPanel
            id="policy"
            what="policy"
            message={policyMsg}
            recipient={policyRecipient(name, commitment)}
            fileName={`${name || 'agent'}-policy.signed.json`}
          />
        </li>
        <li id="ag-step-3" data-s={stepOf(3)}>
          <h2>Approve a held call</h2>
          <p className="nc-cap nc-cap-first">
            Copy the four values from <code>nymirad agent approvals</code>. The approval is good for
            ten minutes and for that one request.
          </p>
          <div className="nc-policy-form nc-agent-grid">
            <Field
              label="Approval ID"
              id="a-id"
              value={approval.id}
              onChange={(v) => setApproval({ ...approval, id: v })}
            />
            <Field
              label="Request hash"
              id="a-hash"
              value={approval.requestHash}
              onChange={(v) => setApproval({ ...approval, requestHash: v })}
            />
            <Field
              label="Amount (micro-USD)"
              id="a-amount"
              value={approval.amountMicro}
              onChange={(v) => setApproval({ ...approval, amountMicro: v })}
            />
            <Field
              label="Nonce"
              id="a-nonce"
              value={approval.nonce}
              onChange={(v) => setApproval({ ...approval, nonce: v })}
            />
          </div>
          <SignPanel
            id="approval"
            what="approval"
            message={approvalMsg}
            recipient={approvalRecipient(name, commitment)}
            fileName={`${name || 'agent'}-approval.signed.json`}
          />
        </li>

        <li id="ag-step-4" data-s={stepOf(4)}>
          <h2>Commands and tool settings</h2>
          <p className="nc-cap nc-cap-first">
            Nothing here runs. Copy a block and run it yourself. No block holds a passphrase or a
            token; the agent reads its token from the file you name.
          </p>
          <ClientGrid pick={pick} onPick={choose} />
          <div className="nc-policy-form nc-agent-grid">
            <label className="nc-field">
              <span>Transport</span>
              <select
                data-testid="c-transport"
                value={cmd.transport}
                onChange={(e) => setCmd({ ...cmd, transport: e.target.value as 'tcp' | 'unix' })}
              >
                <option value="tcp">tcp (loopback port 8788)</option>
                <option value="unix">unix (another user on this machine)</option>
              </select>
            </label>
            {cmd.transport === 'unix' && (
              <Field
                label="Agent user"
                id="c-user"
                value={cmd.agentUser}
                onChange={(v) => setCmd({ ...cmd, agentUser: v })}
              />
            )}
            <Field
              label="Token file path"
              id="c-token"
              value={cmd.tokenFile}
              onChange={(v) => setCmd({ ...cmd, tokenFile: v })}
            />
            <Field
              label="Funding key file path"
              id="c-key"
              value={cmd.keyFile}
              onChange={(v) => setCmd({ ...cmd, keyFile: v })}
            />
            <Field
              label="Amount (base units, 1 USDC is 1000000)"
              id="c-amount"
              value={cmd.amount}
              onChange={(v) => setCmd({ ...cmd, amount: v })}
            />
            <Field
              label={cmd.transport === 'unix' ? 'Agent socket path' : 'Loopback address (optional)'}
              id="c-endpoint"
              value={cmd.endpoint}
              onChange={(v) => setCmd({ ...cmd, endpoint: v })}
            />
            <Field
              label="Agent host for an SSH forward (optional)"
              id="c-host"
              value={cmd.agentHost}
              onChange={(v) => setCmd({ ...cmd, agentHost: v })}
            />
          </div>
          {!blocks.ok && <Errors list={blocks.errors} id="command-errors" />}
          {blocks.ok && (
            <ol className="nc-cmds">
              {blocks.blocks.map((b) => (
                <CopyBlock key={b.id} {...b} />
              ))}
            </ol>
          )}
        </li>

        <li id="ag-step-5" data-s={stepOf(5)}>
          <h2>Recipes</h2>
          <p className="nc-cap nc-cap-first">
            The IronClaw and Cursor recipes, filled from the fields above. They are the same text as
            the README sections. Every recipe names the token file and holds no token. Pick a client
            above to move its recipe to the top.
          </p>
          {picked && recipes.ok && !views.some((v) => v.id === picked.id) && (
            <p className="nc-msg" data-tone="warn">
              Nymira has no {picked.name} recipe yet. Any client that takes an OpenAI base URL and
              key can use the values in the commands above, with chat completions only.
            </p>
          )}
          {!recipes.ok && <Errors list={recipes.errors} id="recipe-errors" />}
          {views.map((v) => {
            const client = CLIENTS.find((c) => c.id === v.id);
            return (
              <Card
                key={v.id}
                title={v.title}
                testId={`recipe-${v.id}`}
                aside={
                  <>
                    {client && <ClientMark client={client} size={24} />}
                    {v.id === pick && <Chip tone="live">picked</Chip>}
                  </>
                }
              >
                <div className="nc-recipe">
                  {v.blocks.map((b, i) => {
                    if (b.t === 'p') return <p key={i}>{plainText(b.text)}</p>;
                    if (b.t === 'ul')
                      return (
                        <ul key={i} className="nc-list">
                          {b.items.map((item) => (
                            <li key={item}>{plainText(item)}</li>
                          ))}
                        </ul>
                      );
                    return (
                      <ol key={i} className="nc-cmds">
                        <CopyBlock
                          id={`recipe-${v.id}-${i}`}
                          title={b.title ?? v.title}
                          text={b.text}
                        />
                      </ol>
                    );
                  })}
                </div>
              </Card>
            );
          })}
        </li>
      </ol>
    </Screen>
  );
}
