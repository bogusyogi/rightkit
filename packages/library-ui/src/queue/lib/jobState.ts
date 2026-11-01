// Queue-state reconciliation helpers shared by App.tsx and useEngineEvents.
//
// Invariants these enforce (audit fixes):
//  - A terminal stage (done/error/cancelled) never regresses to a live one.
//  - Events that arrive before the `add_jobs` response are buffered per id
//    and folded in when the row is inserted, instead of being dropped.
//  - An awaited snapshot is a base, not "newest": rows touched by events or
//    inserts since the request started keep their live values.
//  - The `cancelling` flag clears once a terminal state is acknowledged.

import type { DisplayJob, JobEvent, Stage } from "../contract";

export function isTerminalStage(stage: Stage): boolean {
  return stage === "done" || stage === "error" || stage === "cancelled";
}

/** Apply one JobEvent to a row. Returns `cur` unchanged when the update
 *  would regress a terminal row to a non-terminal stage. */
export function applyJobEvent(cur: DisplayJob, ev: JobEvent): DisplayJob {
  if (isTerminalStage(cur.stage) && !isTerminalStage(ev.stage)) {
    return cur;
  }
  return {
    ...cur,
    stage: ev.stage,
    pct: ev.pct,
    msg: ev.msg !== undefined ? ev.msg : cur.msg,
    error: ev.error !== undefined ? ev.error : cur.error,
    error_kind: ev.error_kind !== undefined ? ev.error_kind : cur.error_kind,
    needs_auth: ev.needs_auth !== undefined ? ev.needs_auth : cur.needs_auth,
    cancelling: isTerminalStage(ev.stage) ? false : cur.cancelling,
  };
}

const EARLY_EVENT_CAP = 500;

/** Buffer an event for a job the UI hasn't inserted yet (the `add_jobs`
 *  response is still in flight). Bounded so unclaimed ids can't grow the
 *  map forever — oldest entries are evicted first. */
export function bufferEarlyEvent(early: Map<string, JobEvent>, ev: JobEvent): void {
  if (!early.has(ev.id) && early.size >= EARLY_EVENT_CAP) {
    const oldest = early.keys().next().value;
    if (oldest !== undefined) early.delete(oldest);
  }
  early.set(ev.id, ev);
}

/** Insert accepted jobs ahead of current rows: ids already known (an event
 *  or snapshot landed first) are skipped, and early buffered events are
 *  folded into the fresh rows so pre-acceptance progress isn't lost. */
export function insertAccepted(
  live: DisplayJob[],
  added: DisplayJob[],
  early: Map<string, JobEvent>,
): DisplayJob[] {
  const known = new Set(live.map((j) => j.id));
  const fresh: DisplayJob[] = [];
  for (const job of added) {
    if (known.has(job.id)) continue;
    const ev = early.get(job.id);
    early.delete(job.id);
    fresh.push(ev ? applyJobEvent(job, ev) : job);
  }
  return fresh.length ? [...fresh, ...live] : live;
}

/** Merge an awaited snapshot with live rows.
 *
 *  - `touched` holds ids mutated by events/inserts since the snapshot
 *    request started — their live rows win (strictly newer than the
 *    snapshot taken at request time); untouched ids take the snapshot's
 *    row verbatim.
 *  - `removed` holds ids deleted locally since the request started — a
 *    stale snapshot that still lists them must not resurrect the row.
 *  - `early` holds events buffered for ids with no live row yet; they're
 *    consumed (folded into the merged row) wherever the snapshot supplies
 *    the row, so a job that finished before its first snapshot doesn't
 *    stall on a stale stage.
 *  - Live rows absent from the snapshot survive only when touched
 *    (otherwise the backend dropped them — the snapshot is authoritative). */
export function reconcileSnapshot(
  live: DisplayJob[],
  snapshot: DisplayJob[],
  touched: Set<string>,
  removed: Set<string>,
  early: Map<string, JobEvent>,
): DisplayJob[] {
  const byId = new Map(live.map((j) => [j.id, j]));
  const snapshotIds = new Set(snapshot.map((r) => r.id));
  const merged = snapshot
    .filter((row) => !(removed.has(row.id) && !byId.has(row.id)))
    .map((row) => {
      const cur = byId.get(row.id);
      const base = cur && touched.has(row.id) ? cur : row;
      const ev = early.get(row.id);
      if (ev) {
        early.delete(row.id);
        return applyJobEvent(base, ev);
      }
      return base;
    });
  const extras = live.filter((j) => touched.has(j.id) && !snapshotIds.has(j.id));
  return [...extras, ...merged];
}
