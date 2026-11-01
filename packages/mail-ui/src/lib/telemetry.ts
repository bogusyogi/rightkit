// Frontend telemetry firehose. Every IPC call, every Zustand store mutation,
// every toolbar click flows through `trace()` into a `@rightkit/logs`
// LogsClient → `rightkit_logs_write` → the local diagnostics log.
//
// Why full-firehose: the 2026-07-03 "Not Responding" freeze (3 hours, no
// crash dump, no .dmp) was diagnosed from the diagnostics log alone — there
// were no click-level breadcrumbs to reconstruct the sequence of events that
// put the renderer into the unresponsive state. This module makes every FE
// action observable from a single log file so the next freeze is diagnosable
// from logs alone, without a debugger.
//
// Privacy invariants (the privacy page advertises "No telemetry — nothing
// leaves this machine"):
//   1. The firehose is local-only. The LogsClient writes through local
//      RightKit commands and never makes a network call.
//   2. The producer never forwards raw strings the user typed (subjects,
//      bodies, search queries, draft text). `allowedFields` is a closed
//      list: every field name here is intentional, every other field is
//      dropped by the LogsClient before it reaches disk.
//   3. Account identifiers and Gmail label identifiers are still subject to
//      `crate::diag::redact_diag_line` on the BE side as defense in depth.

import { createLogsClient } from "@rightkit/logs";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

// Closed list of content-free fields the LogsClient will write. Adding to
// this list is a privacy decision — only do it when the support signal is
// worth more than the privacy cost.
const ALLOWED_FIELDS = [
  // Folder + view: the most useful freeze signal — which folder was active
  // when the renderer stopped responding.
  "folder",
  "view",
  // Sort + filter UI state.
  "unreadOnly",
  "sortMode",
  // Counts: how many threads were in scope, how many selected.
  "total",
  "selected",
  // Dialog state names (settings tabs, picker open/closed, etc.).
  "dialog",
  // Window / visibility (for "Not Responding" diagnosis).
  "hidden",
  "visible",
  "visibility",
  // Hover durations and perf timings (UI-thread responsiveness).
  "ms",
  "used_mb",
  "limit_mb",
  // Source tag for heartbeat resets ("focus", "visibilitychange", …).
  "source",
  // Error / status codes, never messages.
  "code",
  "kind",
  // Bool for toggle states.
  "on",
  // Bulk action verb (archive / trash / spam / read / unread / star / mute).
  "verb",
  // Search mode (e.g. "subject", "from", "has", "label").
  "searchMode",
  // Which feature surface the click came from (so we can distinguish a click
  // on the sidebar from a click on the toolbar without logging the actual
  // thread/folder names).
  "surface",
  // Toolbar button slot ("trash", "archive", "delete_drafts", …).
  "slot",
  // Pane layout (readingPane left/right/off).
  "readingPane",
  // Sync state names (idle / running / paused).
  "syncState",
  // Event topic and the renderer-relative clock.
  "topic",
  "dt_ms",
];

export const logs = createLogsClient({
  invoke: tauriInvoke,
  allowedFields: ALLOWED_FIELDS,
  maxBufferedEvents: 256,
  flushIntervalMs: 250,
});

export type TraceEventName =
  | "click"
  | "ipc"
  | "store"
  | "error"
  | "perf"
  | "render";

/** Counter so we can compute "since renderer start" without grabbing Date.now()
 *  every call (the freeze window is exactly the gap between two events). */
const T0 = (typeof performance !== "undefined" ? performance.now() : Date.now());
function relMs(): number {
  const now = typeof performance !== "undefined" ? performance.now() : Date.now();
  return Math.round(now - T0);
}

/** Public entry point. Fire-and-forget; never throws; never blocks the
 *  caller. Pass `topic` as a stable dotted identifier — never user data.
 *  `body` fields outside ALLOWED_FIELDS are dropped by the LogsClient. */
export function trace(event: TraceEventName, topic: string, body?: Record<string, unknown>): void {
  logs.log(event === "error" ? "error" : "info", `renderer_${event}`, {
    topic,
    dt_ms: relMs(),
    ...body,
  });
}

/** Content-free performance event (stalls, longtasks, heap samples). Same
 *  allow-list and sink as `trace()`; the event name is a stable identifier. */
export function perfEvent(
  level: "info" | "warn",
  event: string,
  fields?: Record<string, unknown>,
): void {
  logs.log(level, event, { dt_ms: relMs(), ...fields });
}

/** Force a flush. Called by the error boundary right before the user dismisses
 *  a frozen window so the most recent batch of clicks survives. */
export async function flushTelemetry(): Promise<void> {
  await logs.flush();
}

/** Public helper for callers that need to forward a single click + body
 *  pair without going through `trace()` directly (e.g. the toolbar uses it
 *  for symmetry). */
export function clickTrace(slot: string, body?: Record<string, unknown>): void {
  trace("click", slot, body);
}
