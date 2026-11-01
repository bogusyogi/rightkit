// Recently-opened localhost preview URLs. Kept separate from the file recents (which are
// PathBroker-token + on-disk based and don't fit URLs) and shown in the Open-URL dialog so a dev can
// re-pick a server without retyping. localStorage-backed → persists across sessions via WebView2's
// user-data folder. Best-effort: any storage failure degrades to "no history", never throws.

const KEY = "viewright:recent-urls";
const MAX = 8;

export function getRecentUrls(): string[] {
  try {
    const raw = localStorage.getItem(KEY);
    const list = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(list)
      ? list.filter((u): u is string => typeof u === "string").slice(0, MAX)
      : [];
  } catch {
    return [];
  }
}

export function addRecentUrl(url: string): void {
  const u = url.trim();
  if (!u) return;
  try {
    const next = [u, ...getRecentUrls().filter((existing) => existing !== u)].slice(0, MAX);
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* storage unavailable — recents are a convenience, not load-bearing */
  }
}
