// The expiry guard's two switches, kept in this browser (public values). A missing or unreadable
// store means the defaults: the guard on, closing at session end off.
import { getSetting, putSetting } from '../store/db';
import { DEFAULT_SETTINGS, type GuardSettings } from './guard';

export async function loadGuardSettings(): Promise<GuardSettings> {
  try {
    const stored = await getSetting<Partial<GuardSettings>>('guard');
    return {
      enabled: typeof stored?.enabled === 'boolean' ? stored.enabled : DEFAULT_SETTINGS.enabled,
      closeAtSessionEnd:
        typeof stored?.closeAtSessionEnd === 'boolean'
          ? stored.closeAtSessionEnd
          : DEFAULT_SETTINGS.closeAtSessionEnd,
    };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export const saveGuardSettings = (settings: GuardSettings) => putSetting('guard', settings);
