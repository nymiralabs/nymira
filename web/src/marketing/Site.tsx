import { Icon } from './Icon';
import { Showcase } from './Showcase';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Clients } from './Clients';
import { Checks } from './Checks';
import { DayGraph, Ladder, Reach } from './Budget';
import { ExitDays, ExpiryLine, NoteLife, SetSize, Zones } from './Motion';
import { Integrity } from './Integrity';
import { Privacy } from './Privacy';
import { Workflow } from './Workflow';
import { LocalScene } from './Local';
import { LogoBackdrop } from './Visuals';
import { AppConsole, type Tab } from './Console';
import { ThemeToggle } from './ThemeToggle';
import { Wordmark } from './Wordmark';
import { nymiraMark } from './brand';
import { TechnologyStrip } from './TechnologyStrip';
import fontLicense from './fonts/OFL.txt?url';
import brandCredits from './assets/brands/credits.txt?url';
import privateNoteIllustration from './assets/private-note-illustration.webp';
import clientRoutesIllustration from './assets/client-routes.webp';
import localNotesIllustration from './assets/local-notes-illustration.webp';
import privacyBoundaries from './assets/privacy-boundaries.webp';

const source = 'https://github.com/nymiralabs/nymira';
const app = 'https://nymira.pages.dev/';
const navigation = [
  ['How it works', '#how-it-works'],
  ['Privacy', '#privacy'],
  ['Agents', '#agents'],
  ['CLI', '#local'],
  ['Integrity', '#integrity'],
  ['Source', source],
] as const;

function Arrow() {
  return (
    <svg
      className="nm-arrow"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <path d="M2 8h11M9 3l5 5-5 5" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}

function Brand({ animated = false }: { animated?: boolean }) {
  return (
    <a className="nm-brand" href="#home" aria-label="Nymira home" draggable={false}>
      {animated ? (
        <span className="nm-brand-swap">
          <span className="nm-brand-word">
            <Wordmark animated />
          </span>
          <img
            className="nm-brand-mark"
            src={nymiraMark}
            alt=""
            aria-hidden="true"
            draggable={false}
          />
        </span>
      ) : (
        <Wordmark />
      )}
    </a>
  );
}

function Action({
  href,
  children,
  secondary = false,
  onClick,
}: {
  href: string;
  children: ReactNode;
  secondary?: boolean;
  onClick?: (e: React.MouseEvent) => void;
}) {
  return (
    <a
      className={`nm-button ${secondary ? 'nm-button-secondary' : ''}`}
      href={href}
      onClick={onClick}
    >
      {children}
      <Arrow />
    </a>
  );
}

function Section({
  id,
  title,
  description,
  art,
  artHeight = 941,
  children,
}: {
  id: string;
  title: string;
  description: string;
  art?: string;
  artHeight?: number;
  children: ReactNode;
}) {
  return (
    <>
      <hr className="nm-divider" />
      <section className="nm-section" id={id} aria-labelledby={`${id}-title`}>
        <div className={art ? 'nm-section-intro nm-section-intro-art' : 'nm-section-intro'}>
          <div>
            <h2 id={`${id}-title`}>
              <a href={`#${id}`}>{title}</a>
            </h2>
            <p className="nm-section-description">{description}</p>
          </div>
          {art && (
            <img
              src={art}
              alt=""
              aria-hidden="true"
              draggable={false}
              loading="lazy"
              width="1672"
              height={artHeight}
            />
          )}
        </div>
        {children}
      </section>
    </>
  );
}

export function Site() {
  const [theme, setTheme] = useState<'dark' | 'light'>('dark');
  // The page behind the app shows during overscroll; keep it the theme's own colour.
  useEffect(() => {
    document.documentElement.style.backgroundColor = theme === 'dark' ? '#000' : '#fff';
    document.documentElement.style.colorScheme = theme;
  }, [theme]);
  const [menu, setMenu] = useState(false);
  // Design preview only: "Open web app" shows the app screens instead of leaving the page.
  const [view, setView] = useState<'site' | Tab>('site');
  const openApp = (e: React.MouseEvent) => {
    e.preventDefault();
    setMenu(false);
    setView('chat');
    window.scrollTo({ top: 0, behavior: 'auto' });
  };
  const inApp = view !== 'site';
  if (inApp) {
    return (
      <div className="nm-site" data-theme={theme}>
        <AppConsole
          tab={view}
          onTab={setView}
          onExit={() => setView('site')}
          theme={theme}
          onTheme={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
        />
      </div>
    );
  }
  return (
    <div className="nm-site" data-theme={theme}>
      <a className="nm-skip" href="#main-content">
        Skip to content
      </a>
      <div className="nm-announcement">
        <a href={`${source}#readme`}>
          <span className="nm-tag">TESTNET</span>
          <span>Nymira is live on NEAR testnet</span>
          <span className="nm-announcement-more">
            Read the project overview <Arrow />
          </span>
        </a>
      </div>
      <div className="nm-header-wrap">
        <header className="nm-header">
          <Brand animated />
          <nav className="nm-nav" aria-label="Main navigation">
            {navigation.map(([text, href]) => (
              <a key={text} href={href}>
                {text}
              </a>
            ))}
          </nav>
          <ThemeToggle
            theme={theme}
            onTheme={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
          />
          <div className="nm-header-actions">
            <a href="#local">Run locally</a>
            <a className="nm-small-button" href={app} onClick={openApp}>
              Open web app
            </a>
          </div>
          <button
            type="button"
            className="nm-icon-button nm-menu-toggle"
            aria-label={menu ? 'Close menu' : 'Open menu'}
            aria-expanded={menu}
            aria-controls="mobile-navigation"
            onClick={() => setMenu(!menu)}
          >
            <svg width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
              <path
                d={menu ? 'M4 4l12 12M16 4L4 16' : 'M2 5h16M2 10h16M2 15h16'}
                stroke="currentColor"
              />
            </svg>
          </button>
        </header>
        {menu && (
          <nav id="mobile-navigation" className="nm-mobile-nav" aria-label="Mobile navigation">
            {navigation.map(([text, href]) => (
              <a key={text} href={href} onClick={() => setMenu(false)}>
                {text}
              </a>
            ))}
            <a href={app} onClick={openApp}>
              Open web app <Arrow />
            </a>
          </nav>
        )}
      </div>

      <main className="nm-main" id="main-content">
        <section className="nm-hero" id="home" aria-labelledby="hero-title">
          <LogoBackdrop />
          <a className="nm-pill" href="#limits">
            Testnet preview · Test funds only <Arrow />
          </a>
          <h1 id="hero-title">Private credit. On your terms.</h1>
          <p className="nm-hero-description">
            Prepaid AI credit backed by USDC on NEAR. Fund a note, use attested models without a
            Nymira account, and withdraw what you don’t spend.
          </p>
          <div className="nm-actions">
            <Action href={app} onClick={openApp}>
              Open web app
            </Action>
            <Action href="#how-it-works" secondary>
              See how it works
            </Action>
          </div>
          <TechnologyStrip theme={theme} />
        </section>

        <Section
          id="how-it-works"
          title="Give your credit a private note."
          description="Fund a note with USDC. Your device proves it can spend without revealing which deposit it came from. The operator authorizes a lease, and encrypted requests pass through to the model."
          art={privateNoteIllustration}
        >
          <Showcase source={source} />
        </Section>
        <Section
          id="local"
          title="Your machine. Your notes."
          description="Run nymirad on your own machine. Keep note secrets and encrypted backups locally, connect a client, and use Tor for network privacy. The browser app cannot hide your IP address from the operator."
          art={localNotesIllustration}
        >
          <LocalScene />
          <p className="nm-section-link">
            <a href={`${source}#download-nymirad`}>
              Read the setup instructions <Arrow />
            </a>
          </p>
        </Section>
        <Section
          id="agents"
          title="Set a budget. Keep control."
          description="Give an agent its own note and token. Set model limits, a per-call ceiling and a daily budget in your local nymirad. Requests above the confirmation threshold wait for your approval."
        >
          <div className="nm-agent-demo">
            <div className="nm-agent-policy">
              <div className="nm-demo-bar">
                <span>Example agent policy</span>
                <span>Signed with your NEAR account</span>
              </div>
              <dl>
                <div>
                  <dt>Budget note</dt>
                  <dd>research-example</dd>
                </div>
                <div>
                  <dt>Daily limit</dt>
                  <dd>1.00 USD</dd>
                </div>
                <div>
                  <dt>Per-call ceiling</dt>
                  <dd>0.10 USD</dd>
                </div>
                <div>
                  <dt>Confirm above</dt>
                  <dd>0.05 USD</dd>
                </div>
                <div>
                  <dt>Allowed model</dt>
                  <dd>z-ai/glm-5.3-flash</dd>
                </div>
                <div>
                  <dt>Lease bound</dt>
                  <dd>0.10 USD</dd>
                </div>
                <div>
                  <dt>Rate limit</dt>
                  <dd>10 per minute</dd>
                </div>
                <div>
                  <dt>Require TEE</dt>
                  <dd>On</dd>
                </div>
              </dl>
              <Zones />
            </div>
            <div className="nm-approval">
              <span className="nm-mono-label">EXAMPLE REQUEST</span>
              <h3>Approval required</h3>
              <p>
                This call exceeds the confirmation threshold. The agent cannot approve its own
                request.
              </p>
              <dl className="nm-approval-facts">
                <div>
                  <dt>Worst case</dt>
                  <dd>0.08 USD</dd>
                </div>
                <div>
                  <dt>Confirm above</dt>
                  <dd>0.05 USD</dd>
                </div>
                <div>
                  <dt>Budget note</dt>
                  <dd>research-example</dd>
                </div>
              </dl>
              <div>
                <span className="nm-demo-decision">Deny</span>
                <span className="nm-demo-decision nm-demo-decision-filled">Owner approves</span>
              </div>
              <small>Illustration, not an active request</small>
            </div>
          </div>
          <Ladder />
          <div className="nm-agent-more">
            <DayGraph />
            <Reach />
          </div>
          <p className="nm-inline-note">
            A hard daily cap needs a daily limit. A per-call confirmation threshold alone can be
            bypassed with many small calls. In opt-in mode, only the note balance is a hard ceiling.
          </p>
        </Section>
        <Section
          id="clients"
          title="Use the client that fits."
          description="Chat in the browser, run the local daemon, or connect an application through the OpenAI-compatible API or the stdio MCP server. The route decides who sees your IP address."
          art={clientRoutesIllustration}
        >
          <Clients />
          <p className="nm-section-link">
            <a href={`${source}#budget-notes-for-agents`}>
              Read the client guides <Arrow />
            </a>
          </p>
        </Section>
        <Section
          id="funding"
          title="Fund a note. Withdraw the rest."
          description="Credit is prepaid in USDC. Keep your latest encrypted backup: it is what you need to recover the remaining balance."
        >
          <div className="nm-note-life">
            <div className="nm-demo-bar">
              <span>Life of a note</span>
              <span>Testnet values</span>
            </div>
            <NoteLife />
            <div className="nm-note-life-body">
              <div className="nm-life-step">
                <h3>Deposit</h3>
                <p>
                  USDC from a NEAR wallet, in a fixed size of 1 or 5 USDC plus a 0.05 USDC note fee.
                </p>
                <code>Payout account fixed here</code>
              </div>
              <div className="nm-life-step">
                <h3>Spend</h3>
                <p>
                  Each lease reserves 0.10 USDC. When it closes, only the charge leaves the note.
                </p>
                <code>Keep the latest encrypted backup</code>
              </div>
              <div className="nm-life-branches">
                <div className="nm-life-step">
                  <h3>Close with the operator</h3>
                  <p>
                    The operator signs a clearance. The vault pays the balance less a 0.03 USDC
                    close fee.
                  </p>
                </div>
                <div className="nm-life-step">
                  <h3>Exit without the operator</h3>
                  <p>Request the exit, wait seven days, then anyone can finalize it.</p>
                  <ExitDays />
                </div>
              </div>
            </div>
            <p className="nm-life-foot">
              Either way, the balance goes to the note’s payout account.
            </p>
          </div>
          <p className="nm-inline-note">
            The web app connects a NEAR wallet in a sealed frame: Meteor or Intear on testnet.
            Funding from another chain through NEAR Intents is in the mainnet build. The testnet app
            only shows 1Click quotes.
          </p>
        </Section>
        <Section
          id="checks"
          title="Your device checks the model."
          description="Before a prompt leaves your device, the client proves the note can pay and verifies the model’s enclave. nymirad runs the same checks and adds the NVIDIA GPU evidence."
        >
          <Checks />
        </Section>
        <Section
          id="privacy"
          title="See the boundaries. Know the tradeoffs."
          description="Private credit does not mean invisible traffic. Know which parts of a request each participant sees before you fund a note."
          art={privacyBoundaries}
          artHeight={836}
        >
          <Privacy />
          <SetSize />
          <p className="nm-inline-note">
            Timing and usage can link requests. The web app cannot use Tor by itself. Use Tor
            Browser or nymirad for IP protection. Other network participants can still observe
            traffic.
          </p>
        </Section>
        <Section
          id="interfaces"
          title="Connects to your workflow."
          description="An agent’s budget note works through the OpenAI-compatible API and the MCP server alike. Your local nymirad keeps the note and applies one policy to every call."
        >
          <Workflow />
          <p className="nm-section-link">
            <a href={`${source}/tree/main/mcp`}>
              Explore the MCP server <Arrow />
            </a>
          </p>
        </Section>
        <Section
          id="integrity"
          title="Check what you run."
          description="Web releases have signed manifests covering the bundle and its response headers. nymirad verify-web compares what the host serves with that signed release."
        >
          <Integrity />
        </Section>
        <hr className="nm-divider" />
        <section id="limits" className="nm-limits" aria-labelledby="limits-title">
          <span className="nm-tag">TESTNET</span>
          <h2 id="limits-title">Test funds. Real limits.</h2>
          <p>
            The anonymity set is small and made of test notes. The contract has had no independent
            review, and the team can still update its code. Notes expire: keep the latest backup and
            follow the app’s exit deadlines.
          </p>
          <ExpiryLine />
          <dl className="nm-limit-facts">
            <div>
              <dt>Cap on open deposits</dt>
              <dd>100 USDC</dd>
            </div>
            <div>
              <dt>Leaving without the operator</dt>
              <dd>7 day wait</dd>
            </div>
            <div>
              <dt>A note expires</dt>
              <dd>30 days after the deposit</dd>
            </div>
            <div>
              <dt>Lose your latest state</dt>
              <dd>An exit from an older state can be defeated</dd>
            </div>
            <div>
              <dt>The vault code</dt>
              <dd>No independent review. The team’s key can replace it.</dd>
            </div>
          </dl>
          <a href={`${source}#limits`}>
            Read the limits before depositing <Arrow />
          </a>
        </section>
        <section className="nm-cta" aria-labelledby="cta-title">
          <h2 id="cta-title">Try a prepaid note on testnet</h2>
          <p>Start with test USDC, keep your backup, and see how the flow works.</p>
          <div className="nm-actions">
            <Action href={app} onClick={openApp}>
              Open web app
            </Action>
            <Action href={`${source}#readme`} secondary>
              Read the source
            </Action>
          </div>
        </section>
      </main>
      <footer className="nm-footer">
        <div className="nm-footer-top">
          <div className="nm-footer-brand">
            <Brand />
            <p>Private prepaid API credit on NEAR.</p>
            <span className="nm-local-status">Local design preview · Unsigned</span>
          </div>
          <div>
            <h3>Product</h3>
            <a href="#how-it-works">How it works</a>
            <a href="#agents">Agents</a>
            <a href={app} onClick={openApp}>
              Web app
            </a>
          </div>
          <div>
            <h3>Resources</h3>
            <a href={`${source}#readme`}>Documentation</a>
            <a href={`${source}/releases`}>Releases</a>
            <a href="#local">Run locally</a>
          </div>
          <div>
            <h3>Project</h3>
            <a href={source}>Source</a>
            <a href={`${source}/issues`}>Issues</a>
            <a href="#integrity">Integrity</a>
          </div>
          <div>
            <h3>Terms</h3>
            <a href={`${source}/blob/main/TERMS.md`}>Terms of use</a>
            <a href="#privacy">Privacy boundaries</a>
            <a href="#limits">Testnet limits</a>
          </div>
        </div>
        <div className="nm-footer-bottom">
          <span>© 2026 Nymira Labs</span>
          <a href={source}>[ GitHub ]</a>
          <a href={fontLicense}>Font license</a>
          <a href={brandCredits}>Brand credits</a>
        </div>
      </footer>
    </div>
  );
}
