// Time helpers. Instants are UTC ISO strings. Local wall-clock values come from Intl with the
// account's IANA zone. Date keys are "YYYY-MM-DD" in the zone they were computed for.

const DAY_MS = 86_400_000;

const formatterCache = new Map<string, Intl.DateTimeFormat>();
function formatter(key: string, make: () => Intl.DateTimeFormat): Intl.DateTimeFormat {
  let f = formatterCache.get(key);
  if (!f) {
    f = make();
    formatterCache.set(key, f);
  }
  return f;
}

/** Calendar date ("YYYY-MM-DD") of an instant as seen in a zone. */
export function dateKeyInZone(utc: string, timeZone: string): string {
  const f = formatter(`date|${timeZone}`, () =>
    new Intl.DateTimeFormat("en-CA", { timeZone, year: "numeric", month: "2-digit", day: "2-digit" }),
  );
  const parts = f.formatToParts(new Date(utc));
  const get = (type: Intl.DateTimeFormatPartTypes) => parts.find((p) => p.type === type)?.value ?? "00";
  return `${get("year")}-${get("month")}-${get("day")}`;
}

/** Wall-clock time "HH:MM" of an instant in a zone. */
export function timeInZone(utc: string, timeZone: string): string {
  const f = formatter(`time|${timeZone}`, () =>
    new Intl.DateTimeFormat("en-GB", { timeZone, hour: "2-digit", minute: "2-digit", hourCycle: "h23" }),
  );
  return f.format(new Date(utc));
}

/** Zone abbreviation for display, for example "GMT+1" or "EDT". */
export function zoneAbbr(utc: string, timeZone: string): string {
  const f = formatter(`abbr|${timeZone}`, () =>
    new Intl.DateTimeFormat("en-US", { timeZone, timeZoneName: "short" }),
  );
  return f.formatToParts(new Date(utc)).find((p) => p.type === "timeZoneName")?.value ?? timeZone;
}

/** Numeric UTC offset of an instant in a zone, for example "+01:00". */
export function zoneOffset(utc: string, timeZone: string): string {
  const f = formatter(`offset|${timeZone}`, () =>
    new Intl.DateTimeFormat("en-GB", { timeZone, timeZoneName: "longOffset" }),
  );
  const raw = f.formatToParts(new Date(utc)).find((p) => p.type === "timeZoneName")?.value ?? "GMT";
  const offset = raw.replace("GMT", "");
  return offset === "" ? "+00:00" : offset;
}

/** "Tue 13 Oct" for a date key. */
export function dayLabel(dateKey: string): string {
  const f = formatter("dayLabel", () =>
    new Intl.DateTimeFormat("en-GB", { timeZone: "UTC", weekday: "short", day: "numeric", month: "short" }),
  );
  return f.format(new Date(`${dateKey}T12:00:00Z`));
}

/** Full instant label, for example "Tue 13 Oct 09:00 GMT+1 (Europe/Lisbon, +01:00)". */
export function instantLabel(utc: string, timeZone: string): string {
  const key = dateKeyInZone(utc, timeZone);
  return `${dayLabel(key)} ${timeInZone(utc, timeZone)} ${zoneAbbr(utc, timeZone)} (${timeZone}, ${zoneOffset(utc, timeZone)})`;
}

export function dateKeyAdd(dateKey: string, days: number): string {
  const ms = Date.parse(`${dateKey}T00:00:00Z`) + days * DAY_MS;
  return new Date(ms).toISOString().slice(0, 10);
}

/** 0 for Monday through 6 for Sunday. */
export function weekdayIndex(dateKey: string): number {
  const day = new Date(`${dateKey}T00:00:00Z`).getUTCDay();
  return (day + 6) % 7;
}

export function mondayOf(dateKey: string): string {
  return dateKeyAdd(dateKey, -weekdayIndex(dateKey));
}

export function weekDays(mondayKey: string): string[] {
  return Array.from({ length: 7 }, (_, i) => dateKeyAdd(mondayKey, i));
}

export function startOfDateUtc(dateKey: string): string {
  return `${dateKey}T00:00:00Z`;
}

/** Remaining time from now until a target, for example "11 h 50 min". Returns "now" when past. */
export function remaining(targetUtc: string, nowUtc: string): string {
  const ms = Date.parse(targetUtc) - Date.parse(nowUtc);
  if (ms <= 0) return "now";
  const totalMin = Math.ceil(ms / 60_000);
  const hours = Math.floor(totalMin / 60);
  const mins = totalMin % 60;
  if (hours === 0) return `${mins} min`;
  if (hours < 48) return `${hours} h ${mins} min`;
  return `${Math.floor(hours / 24)} days`;
}

/** Whole days from now until a calendar date (UTC midnight), rounded up. Negative when past. */
export function daysUntil(dateKey: string, nowUtc: string): number {
  const ms = Date.parse(`${dateKey}T00:00:00Z`) - Date.parse(nowUtc);
  return Math.ceil(ms / DAY_MS);
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-GB");
}
