/** The hub's web view is destroyed on window close, so a view's selection is kept here (best effort). */
export const readKept = <T,>(key: string): T | null => {
  try {
    const raw = localStorage.getItem(key);
    return raw == null ? null : (JSON.parse(raw) as T);
  } catch {
    return null;
  }
};

export const writeKept = (key: string, value: unknown | null) => {
  try {
    if (value == null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage unavailable: the selection is lost on close, as before.
  }
};
