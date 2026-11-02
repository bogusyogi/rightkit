// Pure helpers for the Settings "Vast.ai key" control. The key is saved through the engine's
// existing `studio.models.worker.action` `set_vast_key` (OS credential store, read back before it
// reports success); the key is never kept in component state after a save or echoed anywhere.

import { hostAppName } from "../appName.js";
export interface VastKeyStatus {
  vast_key_stored: boolean;
}

/** The trimmed key to save, or an error line. Empty input is an error here: removing is a separate action. */
export function parseVastKey(raw: string): { key: string } | { error: string } {
  const key = raw.trim();
  if (!key) return { error: 'Paste your Vast.ai API key first.' };
  if (/\s/.test(key)) return { error: 'A Vast.ai API key has no spaces or line breaks. Paste only the key.' };
  return { key };
}

/** One line for the saved/not-saved state; says what the key is for and what happens without it. */
export function vastKeyStatusText(s: VastKeyStatus | null): string {
  if (s === null) return 'Checking the system keychain…';
  return s.vast_key_stored
    ? `Saved in your system keychain. ${hostAppName()} can destroy a rented worker even when it is started without any environment variable.`
    : 'No key saved. Renting a Vast.ai worker is refused until one is saved, so the app can always shut down what it rents.';
}
