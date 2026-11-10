// The testnet deployment served from this machine (`npm run dev:testnet`): the same chain, vault
// and token as `testnet.ts`, but the app origin is the local dev server. Funding and the Notes
// screen work. The operator answers only the real app origin, so chat and leases do not connect
// from here. Never built for release.
import { profile as testnet } from './testnet';
import type { Profile } from './types';

export const profile: Profile = { ...testnet, appOrigin: 'http://localhost:4174' };
