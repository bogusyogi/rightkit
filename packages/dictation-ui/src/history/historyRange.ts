export type DateFilter = "all" | "today" | "yesterday" | "week";

export function startOfDay(ms: number) {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

export function timeLabel(ms: number) {
  const d = new Date(ms);
  return d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

export function formatClip(ms?: number | null): string | null {
  if (!ms || ms < 1000) return null;
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function dateFilterRange(filter: DateFilter, now = Date.now()) {
  if (filter === "all") return { from_ms: null, until_ms: null };
  const today = startOfDay(now);
  if (filter === "today") return { from_ms: today, until_ms: today + 24 * 3600_000 };
  if (filter === "yesterday") return { from_ms: today - 24 * 3600_000, until_ms: today };
  return { from_ms: now - 7 * 24 * 3600_000, until_ms: null };
}
