import { Icon, type IconName } from './Icon';

type Hop = { name: string; tor?: boolean };

// What each client is, what it holds, and the route its requests take to the operator. The IP
// column follows the README: the web app cannot use Tor, nymirad starts its own.
const CLIENTS: readonly {
  icon: IconName;
  name: string;
  blurb: string;
  credential: string;
  hops: readonly Hop[];
  ip: string;
  hidden: boolean;
}[] = [
  {
    icon: 'browser',
    name: 'Browser',
    blurb: 'Fund notes, chat, and sign agent policies with your NEAR wallet.',
    credential: 'NEAR wallet',
    hops: [{ name: 'Browser' }, { name: 'Operator' }],
    ip: 'The operator and the RPC provider see your IP address.',
    hidden: false,
  },
  {
    icon: 'onion',
    name: 'Tor Browser',
    blurb: 'The same web app, opened in Tor Browser.',
    credential: 'NEAR wallet',
    hops: [{ name: 'Tor Browser' }, { name: 'Tor', tor: true }, { name: 'Operator' }],
    ip: 'Your IP address stays out of the operator’s view.',
    hidden: true,
  },
  {
    icon: 'terminal',
    name: 'nymirad',
    blurb: 'The local daemon. Holds the note secret, writes backups, enforces policy.',
    credential: 'Owner token',
    hops: [{ name: 'nymirad' }, { name: 'Tor', tor: true }, { name: 'Operator' }],
    ip: 'It starts its own Tor, or uses a SOCKS proxy you give it.',
    hidden: true,
  },
  {
    icon: 'swap',
    name: 'OpenAI-compatible API',
    blurb: 'Chat and models, with the agent token as the API key.',
    credential: 'Agent token, nyma_…',
    hops: [
      { name: 'Your tool' },
      { name: 'nymirad' },
      { name: 'Tor', tor: true },
      { name: 'Operator' },
    ],
    ip: 'Goes through nymirad’s Tor. Reached on loopback port 8788 or a unix socket.',
    hidden: true,
  },
  {
    icon: 'plug',
    name: 'MCP over stdio',
    blurb: 'Three tools: nymira_balance, nymira_quote and nymira_ask.',
    credential: 'Agent token, read from a file',
    hops: [
      { name: 'Agent host' },
      { name: 'nymirad' },
      { name: 'Tor', tor: true },
      { name: 'Operator' },
    ],
    ip: 'Goes through nymirad’s Tor. The host and its model vendor see every question and answer.',
    hidden: true,
  },
];

export function Clients() {
  return (
    <div className="nm-clients" role="table" aria-label="Clients and how they reach the operator">
      <div className="nm-clients-head" role="row">
        <span role="columnheader">Client</span>
        <span role="columnheader">Credential</span>
        <span role="columnheader">Route to the operator</span>
      </div>
      {CLIENTS.map((c) => (
        <div className="nm-clients-row" role="row" key={c.name}>
          <div className="nm-clients-name" role="cell">
            <Icon name={c.icon} size={24} />
            <span>
              <strong>{c.name}</strong>
              <small>{c.blurb}</small>
            </span>
          </div>
          <code className="nm-clients-credential" role="cell">
            {c.credential}
          </code>
          <div className="nm-clients-route" role="cell">
            <ol>
              {c.hops.map((hop, i) => (
                <li key={`${hop.name}-${i}`} className={hop.tor ? 'nm-hop-tor' : ''}>
                  {hop.name}
                </li>
              ))}
            </ol>
            <span className={c.hidden ? 'nm-ip-hidden' : 'nm-ip-seen'}>{c.ip}</span>
          </div>
        </div>
      ))}
    </div>
  );
}
