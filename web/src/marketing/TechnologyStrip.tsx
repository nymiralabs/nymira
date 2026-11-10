import nearDark from './assets/brands/near-dark.svg';
import nearLight from './assets/brands/near-light.svg';
import circleDark from './assets/brands/circle-dark.svg';
import circleLight from './assets/brands/circle-light.svg';
import nearAiDark from './assets/brands/near-ai-dark.svg';
import nearAiLight from './assets/brands/near-ai-light.svg';
import ethereumDark from './assets/brands/ethereum-dark.svg';
import ethereumLight from './assets/brands/ethereum-light.svg';
import torDark from './assets/brands/tor-dark.svg';
import torLight from './assets/brands/tor-light.svg';
import nvidiaDark from './assets/brands/nvidia-dark.svg';
import nvidiaLight from './assets/brands/nvidia-light.svg';

const technologies = [
  {
    name: 'NEAR',
    purpose: 'Blockchain',
    href: 'https://near.org/',
    dark: nearDark,
    light: nearLight,
    width: 78,
    height: 19,
  },
  {
    name: 'Circle',
    purpose: 'Issuer of USDC, the prepaid credit currency',
    href: 'https://www.circle.com/usdc',
    dark: circleDark,
    light: circleLight,
    width: 76,
    height: 20,
  },
  {
    name: 'NEAR AI',
    purpose: 'Model inference',
    href: 'https://near.ai/',
    dark: nearAiDark,
    light: nearAiLight,
    width: 90,
    height: 17,
  },
  {
    name: 'Ethereum',
    purpose: 'Zero-knowledge circuits reused from zkapi',
    href: 'https://github.com/ethereum/zkapi',
    dark: ethereumDark,
    light: ethereumLight,
    width: 28,
    height: 28,
    label: 'zkapi',
  },
  {
    name: 'Tor',
    purpose: 'Network privacy for the local client',
    href: 'https://www.torproject.org/',
    dark: torDark,
    light: torLight,
    width: 48,
    height: 29,
  },
  {
    name: 'NVIDIA',
    purpose: 'GPU attestation evidence for model inference',
    href: 'https://www.nvidia.com/',
    dark: nvidiaDark,
    light: nvidiaLight,
    width: 116,
    height: 22,
  },
] as const;

export function TechnologyStrip({ theme }: { theme: 'dark' | 'light' }) {
  return (
    <nav className="nm-technology-strip" aria-label="Technology used by Nymira">
      {technologies.map((item) => (
        <a
          key={item.name}
          href={item.href}
          aria-label={`${item.name}: ${item.purpose}`}
          draggable={false}
        >
          <img
            src={theme === 'dark' ? item.dark : item.light}
            width={item.width}
            height={item.height}
            alt={item.name}
            draggable={false}
          />
          {'label' in item && <span className="nm-technology-name">{item.label}</span>}
        </a>
      ))}
    </nav>
  );
}
