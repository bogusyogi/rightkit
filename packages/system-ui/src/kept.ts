import { systemHost } from "./host.js";

const full = (key: string) => `${systemHost().storagePrefix}${key}`;

/** The web view may be destroyed on window close, so a view's selection is kept here (best effort), under the host's storage prefix. */
export const readKept = <T,>(key: string): T | null => {
  try {
    const raw = localStorage.getItem(full(key));
    return raw == null ? null : (JSON.parse(raw) as T);
  } catch {
    return null;
  }
};

export const writeKept = (key: string, value: unknown | null) => {
  try {
    if (value == null) localStorage.removeItem(full(key));
    else localStorage.setItem(full(key), JSON.stringify(value));
  } catch {
    // Storage unavailable: the selection is lost on close, as before.
  }
};
