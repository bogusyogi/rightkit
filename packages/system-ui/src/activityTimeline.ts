import { useEffect, useRef, useState } from "react";
import type { ActivityPage, ActivityPageEntry, ActivityTotal, SystemClient } from "./api.js";
import { systemHost } from "./host.js";
import { clearVisibleInterval, visibleInterval } from "./visible.js";
import { useActivityLease } from "./activityLease.js";

const PAGE = 50;
const REFRESH_MS = 30_000;
export const activityRowKey = (row: ActivityPageEntry["row"]) => row.id;

interface Snapshot {
  entries: ActivityPageEntry[];
  cursor: string | null;
  totals: ActivityTotal[];
  unreadable: string[];
}

function validate(page: ActivityPage, previous: string | undefined, limit: number) {
  if (page.entries.length > limit) throw new Error("Activity page exceeds its limit");
  for (const entry of page.entries) {
    if (!entry.row.id || !entry.position || !entry.cursor || (previous && entry.position >= previous)) {
      throw new Error("Activity page is out of order");
    }
    previous = entry.position;
  }
  if (page.next_cursor !== null && page.next_cursor !== page.entries.at(-1)?.cursor) {
    throw new Error("Invalid activity continuation");
  }
}

// A refreshed cohort of identical old events has no per-copy correspondence to the
// previous cohort. Read through the whole boundary cohort when restoring visible depth.
function boundary(position: string | undefined) {
  return position?.split("#", 1)[0];
}

async function readWindow(client: SystemClient, previous: Snapshot | null, append: boolean, live: () => boolean) {
  if (!client.activityPage) throw new Error("Host does not support cursor-based activity");
  const entries = append ? [...(previous?.entries ?? [])] : [];
  const retained = boundary(previous?.entries.at(-1)?.position);
  let cursor = append ? previous?.cursor ?? null : null;
  let totals = previous?.totals ?? [];
  let first = true;
  for (;;) {
    const limit = first ? PAGE : 200;
    const result = await client.activityPage(limit, cursor);
    if (!live() || "expired" in result) return null;
    validate(result, entries.at(-1)?.position, limit);
    if (first && !append) totals = result.totals;
    // Preserve the previous complete data on a partial refresh.
    if (!append && previous?.entries.length && result.unreadable.length) {
      return {...previous, unreadable: result.unreadable};
    }
    // Older pages belong to one frozen read. Preserve warnings from a newer partial refresh.
    const unreadable = append ? [...new Set([...(previous?.unreadable ?? []), ...result.unreadable])] : result.unreadable;
    for (const entry of result.entries) {
      if (!append && retained && entries.length >= PAGE && entry.position < retained) {
        return {entries, totals, unreadable, cursor: entries.at(-1)!.cursor};
      }
      entries.push(entry);
    }
    cursor = result.next_cursor;
    if (append || !cursor || !retained || entries.at(-1)?.position === retained) {
      return {entries, totals, unreadable, cursor};
    }
    first = false;
  }
}

export function useActivityTimeline() {
  useActivityLease();
  const [data, setData] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const older = useRef<(() => void) | null>(null);
  useEffect(() => {
    let mounted = true;
    let running = false;
    let current: Snapshot | null = null;
    const client = systemHost().client;
    const live = () => mounted;
    async function read(append: boolean) {
      if (running || !mounted || (append && !current?.cursor)) return;
      running = true;
      setBusy(true);
      try {
        let result = await readWindow(client, current, append, live);
        if (!result && mounted) {
          // The engine restarted or evicted the snapshot. Rebuild the visible window
          // before applying an older-page request, at most once per user action.
          result = await readWindow(client, current, false, live);
          if (append && result?.cursor && mounted) {
            result = await readWindow(client, result, true, live);
          }
        }
        if (!mounted) return;
        if (!result) throw new Error("Activity history changed while reloading");
        current = result;
        setData(result);
        setError(null);
      } catch (failure) {
        if (mounted) setError(String(failure));
      } finally {
        running = false;
        if (mounted) setBusy(false);
      }
    }
    older.current = () => void read(true);
    void read(false);
    const timer = visibleInterval(() => void read(false), REFRESH_MS);
    return () => { mounted = false; older.current = null; clearVisibleInterval(timer); };
  }, []);
  return {
    rows: data?.entries.map(entry => entry.row) ?? null,
    unreadable: data?.unreadable ?? [], totals: data?.totals ?? [],
    more: Boolean(data?.cursor), busy, error,
    loadOlder: () => older.current?.(),
  };
}
