import { useEffect, useRef, useState } from 'react';

// The state directory of nymirad, file by file. Every line follows the code (secrets.rs, sealed.rs,
// agents.rs, token.rs, ownersock.rs, backup.rs, config.rs), the README and THREAT_MODEL.md.
type Entry = {
  name: string;
  tag: string;
  holds: string;
  guard: string;
  leaves: { stays: boolean; text: string };
  copied: string;
};

const FILES: readonly Entry[] = [
  {
    name: 'notes.sealed',
    tag: 'sealed',
    holds: 'The secret of every note you own.',
    guard:
      'Sealed with XChaCha20-Poly1305 under a key from your passphrase. nymirad reads the passphrase from the terminal, stdin or a 0600 file, never from argv or the environment.',
    leaves: { stays: true, text: 'Never. The operator does not receive a note secret.' },
    copied: 'Unreadable without the passphrase.',
  },
  {
    name: 'agents.json',
    tag: '0600',
    holds: 'Each agent, its budget note and a hash of its token.',
    guard: 'Mode 0600. serve reads it on every request, so a disabled token stops at once.',
    leaves: { stays: true, text: 'Never. Tokens are not stored, only their hashes.' },
    copied: 'A hash is not a token. It cannot be used to call nymirad.',
  },
  {
    name: 'owner.token',
    tag: '0600',
    holds: 'Your own token for the local API: nymo_ and 32 random bytes.',
    guard:
      'Mode 0600, and nymirad refuses a file others can read. Close, exit and claim go through the owner socket in a 0700 directory, so the file alone does not move money.',
    leaves: { stays: true, text: 'Never.' },
    copied:
      'Any process of your OS user can read it. Run an agent as another user, or on its own nymirad instance.',
  },
  {
    name: 'config.json',
    tag: 'settings',
    holds:
      'The operator and RPC addresses, your Tor package or SOCKS port, the backup places and the backup public key.',
    guard: 'No secrets in it. The backup entry is a public key and the key-derivation settings.',
    leaves: { stays: true, text: 'Never.' },
    copied: 'Safe to read. It shows where your backups go.',
  },
  {
    name: 'attestation-policy.json',
    tag: 'signed',
    holds: 'The NEAR AI deployments you trust: measurements and TCB status.',
    guard:
      'Signed. nymirad checks it against the signer key and the minimum version built into the release.',
    leaves: { stays: true, text: 'Never.' },
    copied: 'Public. Nothing in it is yours.',
  },
  {
    name: 'tor/package/tor',
    tag: 'pinned',
    holds: 'The Tor executable nymirad starts.',
    guard:
      'nymirad runs only a Tor whose SHA-256 is compiled into the build. Any other hash is refused.',
    leaves: {
      stays: true,
      text: 'Never. It is the route out: everything nymirad sends goes through it.',
    },
    copied: 'Skip it with your own SOCKS proxy. Then you check the executable yourself.',
  },
  {
    name: 'state.age',
    tag: 'backup',
    holds: 'Your latest state: notes and wallets, encrypted. Kept outside this folder.',
    guard:
      'age (X25519). With a passphrase, the key comes from Argon2id (64 MiB, 3 passes) and the passphrase needs at least 12 characters.',
    leaves: {
      stays: false,
      text: 'Yes, to the places you chose at setup. It is the only note data that leaves the folder.',
    },
    copied:
      'Your age key or passphrase restores the notes. Treat either as you would the notes themselves.',
  },
];

function Lock({ open = false }: { open?: boolean }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <rect x="3" y="7" width="10" height="7" stroke="currentColor" />
      <path
        d={open ? 'M5.5 7V5a2.5 2.5 0 0 1 4.9-.7' : 'M5.5 7V5a2.5 2.5 0 0 1 5 0v2'}
        stroke="currentColor"
      />
    </svg>
  );
}

export function LocalScene() {
  const reduced =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const [selected, setSelected] = useState(0);
  const [shown, setShown] = useState(reduced ? FILES.length : 0);
  const holder = useRef<HTMLDivElement>(null);
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    const element = holder.current;
    if (!element || reduced) return;
    let timer = 0;
    const observer = new IntersectionObserver(([entry]) => {
      if (!entry?.isIntersecting) return;
      observer.disconnect();
      timer = window.setInterval(() => {
        setShown((n) => {
          if (n + 1 >= FILES.length) window.clearInterval(timer);
          return n + 1;
        });
      }, 110);
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      window.clearInterval(timer);
    };
  }, [reduced]);

  const move = (to: number) => {
    const next = (to + FILES.length) % FILES.length;
    setSelected(next);
    buttons.current[next]?.focus();
  };
  const file = FILES[selected] ?? FILES[0]!;

  return (
    <div className="nm-dir" ref={holder}>
      <div className="nm-dir-bar">
        <code>~/.nymira</code>
        <span>
          <code>drwx------</code> only your user enters
        </span>
      </div>
      <div className="nm-dir-body">
        <ul
          className="nm-dir-list"
          aria-label="Files in the state directory"
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') (e.preventDefault(), move(selected + 1));
            if (e.key === 'ArrowUp') (e.preventDefault(), move(selected - 1));
          }}
        >
          {FILES.map((f, i) => (
            <li key={f.name} data-in={i < shown ? 'true' : 'false'}>
              <button
                type="button"
                ref={(el) => {
                  buttons.current[i] = el;
                }}
                aria-pressed={i === selected}
                tabIndex={i === selected ? 0 : -1}
                onClick={() => setSelected(i)}
              >
                <span className={`nm-dir-lock ${f.leaves.stays ? '' : 'nm-dir-lock-out'}`}>
                  <Lock open={!f.leaves.stays} />
                </span>
                <code>{f.name}</code>
                <span className="nm-dir-tag">{f.tag}</span>
              </button>
            </li>
          ))}
        </ul>
        <div className="nm-dir-view" key={file.name} aria-live="polite">
          <h3>
            <code>{file.name}</code>
          </h3>
          <dl>
            <div>
              <dt>Holds</dt>
              <dd>{file.holds}</dd>
            </div>
            <div>
              <dt>Protected by</dt>
              <dd>{file.guard}</dd>
            </div>
            <div data-leaves={file.leaves.stays ? 'stays' : 'out'}>
              <dt>Leaves the machine</dt>
              <dd>{file.leaves.text}</dd>
            </div>
            <div>
              <dt>If someone copies it</dt>
              <dd>{file.copied}</dd>
            </div>
          </dl>
        </div>
      </div>
      <p className="nm-dir-foot">
        <code>
          <i>$ </i>nymirad serve
        </code>
        <span>
          Everything nymirad sends goes through Tor, and it asks before any direct send. The browser
          app has no such directory and cannot hide your IP address.
        </span>
      </p>
    </div>
  );
}
