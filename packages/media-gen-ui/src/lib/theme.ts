import { createThemeStore, type ThemeChoice, type ThemeStorage } from '@rightkit/react-shell/react';
import { settingsBackend } from './settings-backend';

export type { ThemeChoice };

const THEME_KEY = 'appearance.theme';

/** `?qaTheme=light|dark`: browser QA only, compiled out of release builds. */
function qaTheme(): ThemeChoice | undefined {
  if (!import.meta.env.DEV) return undefined;
  try {
    const qa = new URLSearchParams(window.location.search).get('qaTheme');
    return qa === 'light' || qa === 'dark' ? qa : undefined;
  } catch {
    return undefined;
  }
}

// The theme store reads synchronously, so it reads a memory copy that `hydrateTheme` fills from the
// settings store at startup; every save writes through to the settings store (errors are logged).
const memory = new Map<string, string>();
const settingsStorage: ThemeStorage = {
  getItem: (key) => memory.get(key) ?? null,
  setItem: (key, value) => {
    memory.set(key, value);
    settingsBackend.patch({ [THEME_KEY]: value }).catch((e) => console.error('[genright] theme not saved', e));
  },
  removeItem: (key) => {
    memory.delete(key);
    settingsBackend.patch({ [THEME_KEY]: null }).catch((e) => console.error('[genright] theme not saved', e));
  },
};

/** The single owner of the light/dark/system choice, persisted as `appearance.theme` in the settings store. */
export const themeStore = createThemeStore({ storageKey: 'genright.theme', storage: settingsStorage, qaOverride: qaTheme() });

/** Loads the saved choice from the settings store and applies it. A store failure is logged. */
export async function hydrateTheme(): Promise<void> {
  try {
    const v = (await settingsBackend.load()).values[THEME_KEY];
    if (v === 'light' || v === 'dark' || v === 'system') memory.set('genright.theme', v);
  } catch (e) {
    console.error('[genright] theme not loaded', e);
  }
  themeStore.apply(themeStore.read());
}
