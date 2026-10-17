/**
 * "just now" / "N minutes ago" / "N hours ago" / "N days ago" for a past epoch-millisecond timestamp. Ported from
 * ViewRight's `relativeTimeFrom`, with `now` injectable for tests. A future timestamp reads as "just now".
 */
export function relativeTimeFrom(ms: number, now: number = Date.now()): string {
  const deltaSeconds = Math.max(0, Math.round((now - ms) / 1000));
  if (deltaSeconds < 60) return "just now";
  const minutes = Math.round(deltaSeconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}
