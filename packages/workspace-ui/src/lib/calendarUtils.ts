import type { CalEvent } from "../client.js";

export const DAY = 86400000;
export const WEEKDAY_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** RRULE BYDAY codes with their short labels, Monday first (the event form's repeat-on picker). */
export const WEEKDAYS: ReadonlyArray<readonly [string, string]> = [
  ["MO", "Mon"],
  ["TU", "Tue"],
  ["WE", "Wed"],
  ["TH", "Thu"],
  ["FR", "Fri"],
  ["SA", "Sat"],
  ["SU", "Sun"],
];

export const startOfDay = (ms: number) => {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
};

export const startOfWeek = (ms: number) => {
  const d = new Date(startOfDay(ms));
  return d.getTime() - d.getDay() * DAY;
};

export const sameDay = (a: number, b: number) => startOfDay(a) === startOfDay(b);

export function rangeFor(cursor: number, view: "month" | "week" | "day"): [number, number] {
  if (view === "day") return [startOfDay(cursor), startOfDay(cursor) + DAY];
  if (view === "week") {
    const s = startOfWeek(cursor);
    return [s, s + 7 * DAY];
  }
  const d = new Date(cursor);
  const first = new Date(d.getFullYear(), d.getMonth(), 1).getTime();
  const gridStart = startOfWeek(first);
  return [gridStart, gridStart + 42 * DAY];
}

export function eventDay(e: CalEvent): number {
  if (e.all_day) {
    const [y, m, d] = e.start.slice(0, 10).split("-").map(Number);
    return new Date(y, (m || 1) - 1, d || 1).getTime();
  }
  return startOfDay(new Date(e.start).getTime());
}

export const hhmm = (s: string) => new Date(s).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
