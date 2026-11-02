import { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Select } from "@rightkit/react-shell/react";
import { api, bytes, isWindows, type ActivityRow, type ActivityTimeline } from "../api.js";
import { clearVisibleInterval, visibleInterval } from "../visible.js";
import { useActivityLease } from "../activityLease.js";

/** Rows per page, both for the newest page and for each "Load older". */
const PAGE = 50;
/** Refresh while the section is open. Never faster than 30 seconds. */
const REFRESH_MS = 30_000;

const SECTION_TITLES: Record<string, string> = {
  cleanup: "Cleanup",
  apps: "Apps",
  storage: "Storage",
  chat: "Chat",
};

const KIND_ORDER: ActivityRow["kind"][] = ["Cleanup", "Undo", "Uninstall", "App update", "Drive health", "Scan", "Send", "Chat", "Remote", "File index", "Hub"];

const startOfDay = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate());

/** "Today", "Yesterday", or the date. */
function dayLabel(at: number, now: Date): string {
  const day = startOfDay(new Date(at * 1000));
  const today = startOfDay(now);
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (day.getTime() === today.getTime()) return "Today";
  if (day.getTime() === yesterday.getTime()) return "Yesterday";
  return day.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric", year: "numeric" });
}

const clock = (at: number) => new Date(at * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });

const rowKey = (row: ActivityRow) => `${row.at}|${row.kind}|${row.summary}`;

/** Totals for one UTC period, as the engine reads them over the merged records. */
interface ActivityTotal {
  period: "week" | "month";
  /** Unix seconds, UTC: the start of the period (inclusive). */
  from: number;
  /** Unix seconds, UTC: when the timeline was read. */
  to: number;
  events: number;
  transfers: number;
  moved_bytes: number;
}

type TimelineWithTotals = ActivityTimeline & { totals?: ActivityTotal[] };

const TRASH = isWindows ? "Recycle Bin" : "Trash";
const utc = (at: number) => `${new Date(at * 1000).toISOString().slice(0, 16).replace("T", " ")} UTC`;
const TOTAL_TITLES: Record<ActivityTotal["period"], string> = { week: "This week", month: "This month" };

export function Activity({ onNavigate }: { onNavigate: (section: string) => void }) {
  const [rows, setRows] = useState<ActivityRow[] | null>(null);
  const [unreadable, setUnreadable] = useState<string[]>([]);
  const [totals, setTotals] = useState<ActivityTotal[]>([]);
  const [more, setMore] = useState(false);
  const [filter, setFilter] = useState("All");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** Replace the newest page, keeping older rows the user has already loaded. */
  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const page: TimelineWithTotals = await api.activityTimeline(PAGE, null);
      const oldest = page.rows.length ? page.rows[page.rows.length - 1].at : null;
      setRows((previous) => {
        const older = oldest === null ? [] : (previous ?? []).filter((row) => row.at < oldest);
        return [...page.rows, ...older];
      });
      setUnreadable(page.unreadable);
      setTotals(page.totals ?? []);
      setMore((previous) => previous || page.rows.length >= PAGE);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = visibleInterval(() => void refresh(), REFRESH_MS);
    return () => clearVisibleInterval(id);
  }, [refresh]);

  // Live-activity lease (#448): the notch's session monitors poll every 1 s while this page is visible.
  useActivityLease();

  const loadOlder = async () => {
    if (!rows || rows.length === 0) return;
    setBusy(true);
    try {
      // `before` is exclusive; one second past the oldest row shown brings back its
      // whole second, and rows already shown are dropped below.
      const last = rows[rows.length - 1];
      const page: TimelineWithTotals = await api.activityTimeline(PAGE, last.at + 1);
      setRows((previous) => {
        const seen = new Set((previous ?? []).map(rowKey));
        return [...(previous ?? []), ...page.rows.filter((row) => !seen.has(rowKey(row)))];
      });
      setUnreadable(page.unreadable);
      setMore(page.rows.length >= PAGE);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const kinds = useMemo(() => KIND_ORDER.filter((kind) => (rows ?? []).some((row) => row.kind === kind)), [rows]);
  const current = kinds.includes(filter as ActivityRow["kind"]) ? filter : "All";
  const visible = useMemo(
    () => (rows ?? []).filter((row) => current === "All" || row.kind === current),
    [rows, current],
  );

  const now = new Date();
  const groups: { label: string; rows: ActivityRow[] }[] = [];
  for (const row of visible) {
    const label = dayLabel(row.at, now);
    const last = groups[groups.length - 1];
    if (last && last.label === label) last.rows.push(row);
    else groups.push({ label, rows: [row] });
  }

  return (
    <div className="ck-activity">
      <div className="ck-activity-head">
        <p className="muted">What Pulse did and saw, newest first. Read only: nothing is recorded here.</p>
        <Select
          className="ck-activity-select"
          label="Filter by kind"
          value={current}
          disabled={rows === null}
          onChange={setFilter}
          options={[{ value: "All", label: "All" }, ...kinds.map((kind) => ({ value: kind, label: kind }))]}
        />
      </div>

      {error && <p className="ck-activity-error" role="alert">Activity could not be loaded: {error}</p>}

      {rows === null && !error && <p className="muted">Loading activity…</p>}

      {rows !== null && rows.length === 0 && (
        <div className="ck-activity-empty">
          <strong>Nothing recorded yet</strong>
          <p className="muted">Cleanup runs and undos, uninstalls, drive alerts, storage scans, file transfers and chat messages will appear here as they happen.</p>
        </div>
      )}

      {rows !== null && rows.length > 0 && visible.length === 0 && (
        <p className="muted">No {current.toLowerCase()} records in the rows loaded so far.</p>
      )}

      {totals.length > 0 && current === "All" && (
        <section className="ck-activity-day" aria-label="Totals">
          <h2>Totals</h2>
          <ul className="ck-activity-list">
            {totals.map((t) => (
              <li key={t.period} className="ck-activity-row">
                <span className="ck-activity-kind">{TOTAL_TITLES[t.period]}</span>
                <span className="ck-activity-summary">
                  {t.events} {t.events === 1 ? "record" : "records"}, {t.transfers} {t.transfers === 1 ? "transfer" : "transfers"},{" "}
                  {bytes(t.moved_bytes)} moved to the {TRASH} (not freed). From {utc(t.from)} to {utc(t.to)}.
                </span>
              </li>
            ))}
          </ul>
          <p className="muted ck-activity-note">Totals cover the newest records each source keeps, not every record ever made.</p>
        </section>
      )}

      {groups.map((group) => (
        <section key={group.label} className="ck-activity-day" aria-label={group.label}>
          <h2>{group.label}</h2>
          <ul className="ck-activity-list">
            {group.rows.map((row) => (
              <li key={rowKey(row)} className="ck-activity-row">
                <time className="ck-activity-time" dateTime={new Date(row.at * 1000).toISOString()}>{clock(row.at)}</time>
                <span className="ck-activity-kind">{row.kind}</span>
                <span className="ck-activity-summary">{row.summary}</span>
                {row.open && (
                  <Button size="sm" variant="secondary" onClick={() => onNavigate(row.open as string)}>
                    Open {SECTION_TITLES[row.open] ?? row.open}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </section>
      ))}

      {more && (
        <div className="ck-activity-more">
          <Button size="sm" variant="secondary" onClick={() => void loadOlder()} disabled={busy}>
            Load older
          </Button>
        </div>
      )}

      {unreadable.length > 0 && (
        <p className="muted ck-activity-note">Could not read: {unreadable.join(", ")}.</p>
      )}
    </div>
  );
}
