import { useState } from 'react';
import { buildBlocks, type CommandFields } from '../agents/commands';
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

// This screen builds files and commands from what the owner types. It creates no budget note,
// asks for and shows no token, reads nothing from the URL and makes no request to `nymirad`:
// the only thing it calls is the owner's own wallet, to sign a message.

function Errors({ list, id }: { list: string[]; id: string }) {
  if (list.length === 0) return null;
  return (
    <ul className="error" data-testid={id}>
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
    <label>
      {label}
      <input
        type="text"
        data-testid={id}
        value={value}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
      {hint && <small className="hint"> {hint}</small>}
    </label>
  );
}

function CopyBlock({ id, title, text }: { id: string; title: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div>
      <h4>{title}</h4>
      <pre data-testid={`block-${id}`}>{text}</pre>
      <button
        type="button"
        data-testid={`copy-${id}`}
        onClick={() => {
          navigator.clipboard
            .writeText(text)
            .then(() => setCopied(true))
            .catch(() => setCopied(false));
        }}
      >
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
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
    <div>
      <p>
        You are about to sign this {what} as <strong>{account ?? 'your account'}</strong>, for the
        recipient <code data-testid={`${id}-recipient`}>{recipient}</code>:
      </p>
      <pre data-testid={`${id}-message`}>{message.value}</pre>
      <button
        type="button"
        data-testid={`${id}-sign`}
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
        Sign with {wallet.connected ? 'your wallet' : 'a wallet (connect one first)'}
      </button>
      {error && (
        <p className="error" data-testid={`${id}-error`}>
          {error}
        </p>
      )}
      {file && (
        <div>
          <p>
            Signature: <code data-testid={`${id}-signature`}>{signature}</code>
          </p>
          <pre data-testid={`${id}-file`}>{file}</pre>
          <button
            type="button"
            data-testid={`${id}-download`}
            onClick={() => download(file, fileName, 'application/json')}
          >
            Download {fileName}
          </button>
        </div>
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
  const set = (k: keyof typeof policy) => (v: string) => setPolicy({ ...policy, [k]: v });

  return (
    <section data-testid="agents">
      <h1>Agents</h1>
      <p data-testid="agents-intro">
        A budget note is a note you fund for one agent. Its payout account is your own account,
        fixed at deposit, so whatever the agent leaves comes back to you. The agent gets a token and
        can spend that one note on inference. It cannot withdraw it anywhere but back to you.
      </p>
      <p className="notice" data-testid="agents-scope">
        This page creates no budget note, never asks for or shows a token, and makes no request to{' '}
        <code>nymirad</code>. It writes the policy file, signs it and your approvals with your
        wallet, and prints commands for you to run yourself.
      </p>
      <div data-testid="agents-guarantees">
        <h2>What is guaranteed, and what is not</h2>
        <ul>
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
      </div>

      <h2>1. Wallet</h2>
      <p>
        The policy and each approval are signed as NEP-413 messages with your NEAR wallet. The
        account must be the one that funded the budget note. <code>nymirad</code> checks that the
        signing key is a full-access key of that account when you load the policy.
      </p>
      <WalletConnect />
      {wallet.connected && wallet.connected.signature === 'unsupported' && (
        <p className="hint">This browser cannot check the sign-in signature.</p>
      )}

      <h2>2. The agent</h2>
      <Field label="Agent name" id="agent-name" value={name} onChange={setName} />
      <Field
        label="Budget note commitment"
        id="agent-commitment"
        value={commitment}
        onChange={setCommitment}
        hint="The note field of the agent in agents.json in the nymirad state directory."
      />

      <h2>3. Policy</h2>
      <fieldset>
        <legend>Allowed models</legend>
        {MODELS.map((m) => (
          <label key={m.id}>
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
            />{' '}
            {m.id}
          </label>
        ))}
      </fieldset>
      <Field
        label="Per lease limit (USD)"
        id="p-lease"
        value={policy.maxLeaseUsd}
        onChange={set('maxLeaseUsd')}
        hint="At least 0.1."
      />
      <Field
        label="Per call limit (USD)"
        id="p-call"
        value={policy.maxCallUsd}
        onChange={set('maxCallUsd')}
      />
      <Field
        label="Per day limit (USD, UTC day)"
        id="p-daily"
        value={policy.maxDailyUsd}
        onChange={set('maxDailyUsd')}
      />
      <Field
        label="Ask me above (USD per call)"
        id="p-confirm"
        value={policy.confirmAboveUsd}
        onChange={set('confirmAboveUsd')}
      />
      <Field
        label="Requests a minute"
        id="p-rate"
        value={policy.rateLimitPerMin}
        onChange={set('rateLimitPerMin')}
      />
      <p data-testid="p-tee">
        <code>require_tee: true</code> is the only value this page offers: <code>nymirad</code>{' '}
        releases an answer only after it checked the provider&apos;s signature on it.
      </p>
      <Field
        label="Policy version"
        id="p-version"
        value={version}
        onChange={setVersion}
        hint="Must be higher than any version this agent has had."
      />
      {yaml.ok && <pre data-testid="policy-yaml">{yaml.value}</pre>}
      {!yaml.ok && <Errors list={yaml.errors} id="policy-errors" />}
      <SignPanel
        id="policy"
        what="policy"
        message={policyMsg}
        recipient={policyRecipient(name, commitment)}
        fileName={`${name || 'agent'}-policy.signed.json`}
      />

      <h2>4. Approve a held call</h2>
      <p>
        Copy the four values from <code>nymirad agent approvals</code>. The approval is good for ten
        minutes and for that one request.
      </p>
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
      <SignPanel
        id="approval"
        what="approval"
        message={approvalMsg}
        recipient={approvalRecipient(name, commitment)}
        fileName={`${name || 'agent'}-approval.signed.json`}
      />

      <h2>5. Commands and tool settings</h2>
      <p>
        Nothing here runs. Copy a block and run it yourself. No block holds a passphrase or a token;
        the agent reads its token from the file you name.
      </p>
      <label>
        Transport
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
      {!blocks.ok && <Errors list={blocks.errors} id="command-errors" />}
      {blocks.ok && blocks.blocks.map((b) => <CopyBlock key={b.id} {...b} />)}
    </section>
  );
}
