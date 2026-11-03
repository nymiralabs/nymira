// The build profile of this bundle. `@profile` is an alias set in vite.config.ts and tsconfig.json
// to src/profiles/<mode>.ts, so a bundle contains the constants of its own profile only.
import { profile } from '@profile';
import type { Profile } from './profiles/types';

export const PROFILE: Profile = Object.freeze(profile) as Profile;
