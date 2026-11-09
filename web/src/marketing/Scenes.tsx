import { Icon } from './Icon';
import {
  Bar,
  Boundary,
  Cells,
  Cipher,
  Count,
  Hash,
  HashLine,
  Head,
  Node,
  Row,
  Tag,
  Typed,
  Wave,
  Wires,
  Zone,
  place,
  type Pt,
  type Wire,
} from './Board';

const HASH = '5940b853…ad4f5';

const Tick = ({ spin = false }: { spin?: boolean }) => (
  <span className={spin ? 'nm-working' : 'nm-check'}>
    <Icon name={spin ? 'chevron' : 'check'} size={14} />
  </span>
);

/* ------------------------------------------------------------------ CLI */

export const CLI_STAGES = 8;

export function CliScene({ s }: { s: number }) {
  const events: readonly [string, number][] = [
    ['Request proof created locally', 1],
    ['Lease authorized', 2],
    ['Prompt encrypted to the model', 3],
    ['Encrypted answer received', 4],
    ['Successor state checked and saved', 6],
  ];
  const wires: Wire[] = [
    {
      pts: [
        [296, 150],
        [326, 150],
      ],
      on: s === 0 || s === 4 || s === 5,
      reverse: s >= 4,
    },
    {
      pts: [
        [566, 150],
        [674, 150],
      ],
      on: s === 2 || s === 4,
      reverse: s === 4,
      label: s === 4 ? 'ciphertext' : 'proof + lease',
    },
    {
      pts: [
        [819, 272],
        [819, 312],
      ],
      on: s === 3 || s === 4,
      reverse: s === 4,
    },
    {
      pts: [
        [446, 272],
        [446, 312],
      ],
      on: s === 6,
      dashed: true,
    },
  ];
  return (
    <>
      <Zone box={[16, 36, 570, 468]} label="Your machine" />
      <Zone box={[654, 36, 330, 468]} label="Over the network" />
      <Boundary x={620} y={36} h={468} label="proof · lease · ciphertext" />
      <div
        className={`nm-terminal nm-show-node ${s === 0 || s === 4 ? 'nm-node-active' : ''}`}
        style={place([36, 56, 260, 428])}
      >
        <div className="nm-terminal-title">
          <span>client</span>
          <span>local</span>
        </div>
        <div className="nm-terminal-content">
          <p className="nm-terminal-prompt">$ nymirad serve</p>
          {['Local API', 'Per-agent budgets', 'Encrypted model requests'].map((t) => (
            <p key={t}>
              <Tick /> {t}
            </p>
          ))}
          <p className="nm-terminal-question">
            &gt; <Typed text="Explain how a prepaid note works." run={s === 0} done={s > 0} />
            {s === 0 && <span className="nm-cursor" />}
          </p>
          {events.map(([text, at]) => (
            <p key={text} className={`nm-event ${s >= at ? 'nm-event-visible' : ''}`}>
              <Tick spin={s === at} /> {text}
            </p>
          ))}
          <p className="nm-show-stream">
            <Typed
              text="A prepaid note holds credit you can spend without a Nymira account."
              run={s === 4}
              done={s > 4}
            />
            {s === 4 && <span className="nm-cursor" />}
          </p>
        </div>
      </div>
      <Node box={[326, 56, 240, 216]} on={s === 0 || s === 1 || s === 5}>
        <Head title="nymirad" chip="running" on />
        <span className="nm-node-subtitle">Holds the prepaid note</span>
        <div className="nm-show-ledger">
          <span className="nm-show-ledger-sum">
            <Count value={s >= 5 ? 0.98 : 1} /> USDC
            {s >= 5 && <em className="nm-show-spent">−0.02</em>}
          </span>
          <Cells
            n={10}
            filled={s >= 2 ? 9 : 10}
            leased={s >= 2 && s < 6 ? 1 : 0}
            drain={s >= 5 ? 0.2 : 0}
            part={s >= 6 ? 0.8 : 0}
            className="nm-cells-note"
          />
          <span className="nm-show-ledger-key">
            {s >= 2 && s < 6
              ? '0.10 reserved for the open lease'
              : s >= 6
                ? 'Lease retired · next state signed'
                : 'Balance · one cell is 0.10'}
          </span>
        </div>
        <div className="nm-show-meta">
          <span>Proof, made here</span>
          <span>{s >= 2 ? '100%' : s === 1 ? '…' : '0%'}</span>
        </div>
        <Bar value={s >= 2 ? 1 : s === 1 ? 0.7 : 0} />
        <span className="nm-node-bottom">Secret stays on your device</span>
      </Node>
      <Node box={[326, 312, 240, 172]} on={s === 6}>
        <Head title="Encrypted backup" />
        <span className="nm-node-subtitle">state.age</span>
        <ol className="nm-chain">
          <li className={s >= 6 ? 'nm-chain-old' : ''}>
            <Icon name="lock" size={14} />
            <span>State 1</span>
            <code>older</code>
          </li>
          <li className={`nm-chain-new ${s >= 6 ? 'nm-chain-on' : ''}`}>
            <Icon name={s >= 6 ? 'check' : 'lock'} size={14} />
            <span>State 2</span>
            <code>{s >= 6 ? 'saved' : 'next'}</code>
          </li>
        </ol>
        <span className="nm-node-bottom">Keep your latest state</span>
      </Node>
      <Node box={[674, 56, 290, 216]} on={s === 2 || s === 3 || s === 4}>
        <Head
          title="Operator"
          chip={s >= 2 && s < 6 ? 'lease open' : 'idle'}
          on={s >= 2 && s < 6}
        />
        <span className="nm-node-subtitle">Authorizes the lease. Sees only ciphertext.</span>
        <Cipher run={s >= 3 && s <= 4} rows={3} cols={26} />
        <div className="nm-show-meta">
          <span>Lease bound</span>
          <span>0.10 USDC</span>
        </div>
        <Cells n={10} filled={s >= 5 ? 2 : 0} leased={s >= 2 && s < 6 ? (s >= 5 ? 8 : 10) : 0} />
        <div className="nm-show-meta">
          <span>Charged</span>
          <span>{s >= 5 ? '0.02 USDC' : '·'}</span>
        </div>
      </Node>
      <Node box={[674, 312, 290, 172]} on={s === 3 || s === 4}>
        <Head title="Attested model" chip="NEAR AI" on={s >= 3} />
        <Row on={s >= 3}>
          <Tick /> Attestation checked
        </Row>
        <Row on={s >= 3}>
          <Tick /> Prompt sealed to its key
        </Row>
        <Wave run={s === 3 || s === 4} rows={3} cols={32} />
        <span className="nm-node-bottom">The model reads your prompt</span>
      </Node>
      <Wires wires={wires} />
    </>
  );
}

/* ------------------------------------------------------------------ MCP */

export const MCP_STAGES = 8;

const CHECKS = [
  'Token enabled',
  'Signed policy',
  'Model',
  'Rate',
  'Call cost',
  'The day',
  'Note balance',
] as const;

export function McpScene({ s }: { s: number }) {
  const tool = s === 0 ? 0 : s === 1 ? 1 : 2;
  const checking = s === 1 || s === 4;
  const verdict = s === 1 ? 'needs approval' : s === 2 || s === 3 ? 'held' : s >= 4 ? 'allow' : '';
  const wires: Wire[] = [
    {
      pts: [
        [286, 122],
        [376, 122],
      ],
      on: s <= 2 || s === 4 || s === 6,
      reverse: s === 6,
      label:
        s === 6 ? 'result' : s === 4 ? 'ask + id' : s === 0 ? 'balance' : s === 1 ? 'quote' : 'ask',
    },
    {
      pts: [
        [491, 188],
        [491, 228],
      ],
      on: s === 1 || s === 2 || s === 4,
    },
    {
      pts: [
        [606, 400],
        [716, 400],
      ],
      on: s === 5 || s === 6,
      reverse: s === 6,
      label: s === 6 ? 'answer' : 'sealed',
    },
    {
      pts: [
        [606, 300],
        [666, 300],
        [666, 158],
        [716, 158],
      ],
      on: s === 2 || s === 3,
      dashed: true,
      label: 'held',
    },
  ];
  return (
    <>
      <Zone box={[16, 36, 610, 468]} label="Your machine" />
      <Zone box={[696, 36, 288, 468]} label="Approval and model" />
      <div className="nm-terminal nm-show-node" style={place([36, 56, 250, 428])}>
        <div className="nm-terminal-title">
          <span>agent host</span>
          <span>stdio</span>
        </div>
        <div className="nm-terminal-content">
          <div className={`nm-event ${s >= 0 ? 'nm-event-visible' : ''}`}>
            <p className={s === 0 ? 'nm-working' : ''}>
              <Icon name="chevron" size={14} /> nymira_balance
            </p>
            <p className="nm-show-result">
              <Icon name="right" size={12} /> spendable 1.00 USDC
            </p>
          </div>
          <div className={`nm-event ${s >= 1 ? 'nm-event-visible' : ''}`}>
            <p className={s === 1 ? 'nm-working' : ''}>
              <Icon name="chevron" size={14} /> nymira_quote
            </p>
            <p className="nm-show-result">
              <Icon name="right" size={12} /> needs approval · worst case 0.04
            </p>
          </div>
          <div className={`nm-event ${s >= 2 ? 'nm-event-visible' : ''}`}>
            <p className={s === 2 ? 'nm-working' : ''}>
              <Icon name="chevron" size={14} /> nymira_ask max_usd 0.04
            </p>
            <p className="nm-show-result">
              <Icon name="right" size={12} /> approval_required
            </p>
          </div>
          <div className={`nm-event ${s >= 4 ? 'nm-event-visible' : ''}`}>
            <p className={s >= 4 && s < 7 ? 'nm-working' : ''}>
              <Icon name="chevron" size={14} /> nymira_ask + approval_id
            </p>
            <p className="nm-show-result">
              <Icon name="right" size={12} /> {s >= 6 ? 'untrusted_text · charged 0.021' : '…'}
            </p>
          </div>
          <p className="nm-show-fine">
            The host and its model vendor see every question and answer.
          </p>
        </div>
      </div>
      <Node box={[376, 56, 230, 148]} on={s <= 2 || s === 4}>
        <Head title="nymira MCP server" chip="stdio" on />
        <ul className="nm-show-tools">
          {(
            [
              ['nymira_balance', 'reads'],
              ['nymira_quote', 'reads'],
              ['nymira_ask', 'spends'],
            ] as const
          ).map(([name, kind], i) => (
            <li key={name} className={tool === i && s <= 6 ? 'nm-show-tool-on' : ''}>
              <code>{name}</code>
              <span>{kind}</span>
            </li>
          ))}
        </ul>
      </Node>
      <Node box={[376, 228, 230, 256]} on={checking || s === 2 || s === 3}>
        <Head title="nymirad policy" chip={verdict || 'ready'} on={verdict === 'allow'} />
        <div className="nm-show-checks">
          {CHECKS.map((name, i) => (
            <p
              key={name}
              className={`nm-show-check ${s >= 4 || (s === 1 && i < 4) ? 'nm-show-check-on' : ''} ${i === 4 && s >= 1 && s <= 3 ? 'nm-show-check-held' : ''}`}
              style={{ transitionDelay: `${i * 140}ms` }}
            >
              <Icon name={i === 4 && s >= 1 && s <= 3 ? 'chevron' : 'check'} size={12} /> {name}
            </p>
          ))}
        </div>
        <div className="nm-show-meta">
          <span>Call ceiling</span>
          <span>0.04 USDC</span>
        </div>
        <Cells n={8} filled={s >= 6 ? 4 : 0} leased={s >= 1 ? (s >= 6 ? 4 : 8) : 0} />
        <span className="nm-node-bottom">The agent cannot raise its ceiling</span>
      </Node>
      <div
        className={`nm-hold nm-show-node ${s >= 2 ? 'nm-hold-on' : ''} ${s >= 4 ? 'nm-hold-done' : ''}`}
        style={place([716, 56, 248, 202])}
      >
        <div className="nm-hold-head">
          <span>Held for approval</span>
          <code>approval_required</code>
        </div>
        <p>
          <code>nymira_ask</code> may cost up to <strong>0.04 USDC</strong>.
        </p>
        <div className="nm-hold-action">
          <button
            className={s === 3 ? 'nm-hold-press' : ''}
            type="button"
            tabIndex={-1}
            aria-hidden="true"
          >
            {s >= 4 ? (
              <>
                <Icon name="check" size={14} /> Approved
              </>
            ) : (
              'Approve'
            )}
          </button>
          <svg
            className={`nm-hold-cursor ${s === 3 ? 'nm-hold-cursor-go' : ''}`}
            viewBox="0 0 20 20"
            width="19"
            height="19"
            aria-hidden="true"
          >
            <path d="M3 2.5v13l3.6-3.5 2.4 5 2-1-2.4-4.9 5-.6z" />
          </svg>
        </div>
        <span className="nm-hold-note">Only you can approve. The server cannot.</span>
      </div>
      <Node box={[716, 312, 248, 172]} on={s === 5}>
        <Head title="Attested model" chip="NEAR AI" on={s >= 5} />
        <Row on={s >= 5}>
          <Tick /> Attestation checked
        </Row>
        <Wave run={s === 5 || s === 6} rows={3} cols={28} />
        <span className="nm-node-bottom">Sees only Nymira’s organisation as the payer</span>
      </Node>
      <Wires wires={wires} />
    </>
  );
}

/* --------------------------------------------------------------- Source */

export const SOURCE_STAGES = 8;

const VAULT_SHA = '5940b853e51476c69ab2cd0ffba2fb76ffd3fedb621a2471c9ccd2bdb62ad4f5';
const BUNDLE_SHA = '3ee5ccb57cdaf2a1fa58cd914b6806909eac98c4dd20c013f6e0af5c926e045f';
const HEADERS_SHA = '0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8';

export function SourceScene({ s }: { s: number }) {
  const wires: Wire[] = [
    {
      pts: [
        [250, 140],
        [300, 140],
      ],
      on: s === 0 || s === 1,
    },
    {
      pts: [
        [250, 388],
        [300, 388],
      ],
      on: s === 5,
    },
  ];
  const signed: readonly [string, string, string, number][] = [
    ['bundle_hash', BUNDLE_SHA, 'served', 6],
    ['header_set_hash', HEADERS_SHA, 'served', 7],
  ];
  return (
    <>
      <span className="nm-show-rowlabel" style={place([30, 16, 0, 0])}>
        Contract
      </span>
      <span className="nm-show-rowlabel" style={place([30, 256, 0, 0])}>
        Web app
      </span>
      <Node box={[30, 44, 220, 192]} on={s === 0}>
        <Head title="Source" chip="tag" on={s >= 0} />
        <span className="nm-node-subtitle">github.com/nymiralabs/nymira</span>
        <div className="nm-show-code">
          <p className="nm-event nm-event-visible">
            <span className="nm-dim">$</span> git checkout vault-testnet-1
          </p>
          <p className="nm-event nm-event-visible nm-dim">commit 18107d8…</p>
        </div>
        <span className="nm-node-bottom">cargo-near 0.22.0 · rustc 1.99.0</span>
      </Node>
      <Node box={[300, 44, 670, 192]} on={s >= 1 && s <= 4}>
        <Head
          title="Vault code hash"
          chip={s >= 4 ? 'identical' : s >= 3 ? 'comparing' : s === 1 ? 'building' : 'waiting'}
          on={s >= 4}
        />
        <div className="nm-compare">
          <span>Rebuilt from the tag</span>
          <HashLine value={s >= 2 ? VAULT_SHA : s === 1 ? '·'.repeat(64) : ''} lit={s >= 4} />
          <i className={s >= 4 ? 'nm-compare-on' : ''} aria-hidden="true">
            {'|'.repeat(64)}
          </i>
          <HashLine value={s >= 3 ? VAULT_SHA : ''} lit={s >= 4} />
          <span>On chain · vault.nymira.testnet</span>
        </div>
        <span className="nm-node-bottom">
          scripts/reproducible-smoke.sh · the chain side is read with view_account
        </span>
      </Node>
      <Node box={[30, 280, 220, 216]} on={s === 5}>
        <Head title="Signed release" chip="minisign" on={s >= 5} />
        <span className="nm-node-subtitle">Manifest covers the bundle and its headers</span>
        <div className="nm-show-code">
          <p>
            <span className="nm-dim">bundle_hash</span> signed
          </p>
          <p>
            <span className="nm-dim">header_set_hash</span> signed
          </p>
        </div>
        <span className="nm-node-bottom">key 934C176AF34D794D</span>
      </Node>
      <Node box={[300, 280, 670, 216]} on={s >= 5 && s <= 7}>
        <Head
          title="nymirad verify-web"
          chip={s === 7 ? 'matches' : s >= 5 ? 'checking' : 'idle'}
          on={s === 7}
        />
        <span className="nm-node-subtitle">
          Compares what nymira.pages.dev serves with the signed manifest
        </span>
        <Row on={s >= 5}>
          <Tick /> Signature of manifest.json
        </Row>
        {signed.map(([name, value, , at]) => (
          <div key={name} className="nm-compare nm-compare-row">
            <span>
              {name} · {s >= at ? 'served = signed' : 'signed'}
            </span>
            <HashLine value={s >= 5 ? value : ''} lit={s >= at} size={10} />
            <i className={s >= at ? 'nm-compare-on' : ''} aria-hidden="true">
              {'|'.repeat(64)}
            </i>
            <HashLine value={s >= at ? value : ''} lit={s >= at} size={10} />
          </div>
        ))}
      </Node>
      <Wires wires={wires} />
    </>
  );
}

const HOT: readonly (readonly Pt[])[] = [
  // CLI
  [
    [171, 267],
    [436, 156],
    [820, 156],
    [820, 395],
    [700, 150],
    [436, 156],
    [436, 395],
    [171, 267],
  ],
  // MCP
  [
    [491, 114],
    [491, 355],
    [840, 150],
    [840, 150],
    [491, 355],
    [840, 395],
    [491, 114],
    [161, 267],
  ],
  // Source
  [
    [160, 140],
    [500, 140],
    [500, 140],
    [840, 140],
    [670, 140],
    [160, 386],
    [840, 386],
    [500, 386],
  ],
];

/** Where the lit dots rest for a tab and stage: on the part that is working. */
export const hotSpot = (tab: number, stage: number): Pt => HOT[tab]![stage] ?? [500, 260];
