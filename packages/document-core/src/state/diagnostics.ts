// the host app diagnostics wiring. The persisted renderer trail (ring buffer, legacy-key import,
// dedup, idle/lifecycle flush) is owned by @rightkit/logs `createDiagnosticsRecorder`; this file
// holds only the host app configuration: the static label allowlist, the settings privacy toggle,
// forwarding to the shared native log, and the console accessors. Read via
// `window.vrDiagnostics()` in console.

import {
  createDiagnosticsRecorder,
  createLogsClient,
  installWindowCrashCapture,
  type DiagnosticEntry,
  type DiagnosticInput,
} from "@rightkit/logs";
import { invoke } from "@tauri-apps/api/core";

// Static call-site labels. The recorder drops any label not listed here, and a label must never be
// derived from a path, document name, href or error text: those do not belong in telemetry. Adding
// a call site means adding its label here; the `DiagnosticEvent` type enforces that at compile time.
const DIAGNOSTIC_EVENTS = [
  "app:loaded",
  "app:shown",
  "window:error",
  "window:unhandledrejection",
  "layout:resize-observer-loop",
  "tauri:event-listener-teardown",
  "settings:marker-write-failed",
  "legal-gate:resize-failed",
  "legal-gate:fullscreen-exit-failed",
  "legal-gate:unmaximize-failed",
  "legal-gate:resizable-failed",
  "legal-gate:maximizable-failed",
  "workspace:fullscreen-exit-failed",
  "workspace:resizable-failed",
  "workspace:maximizable-failed",
  "workspace:maximize-failed",
  "window:background-color-failed",
  "window:resize",
  "window:resize-listener-failed",
  "react:boundary",
  "pane:active:paint",
  "open:start",
  "open:unsupported",
  "open:read",
  "open:ready",
  "open:library",
  "open:failed",
  "open:error",
  "print:failed",
  "print:failed:pdf",
  "print:failed:md",
  "print:failed:txt",
  "print:failed:docx",
  "print:failed:docx-no-pages",
  "print:requested",
  "fingerprint:error",
  "save:start",
  "save:pdf",
  "save:image",
  "save:text",
  "save:error",
  "save:conflict",
  "viewer:route:preview",
  "viewer:route:source",
  "viewer:route:interactive",
  "viewer:content:paint",
  "html:mount",
  "html:build",
  "html:block-entry-refused",
  "md:cold-mount",
  "mdx:compile-error",
  "mdx:runtime-error",
  "mdx:link-blocked",
  "docx:blank-render",
  "update:install-failed",
  "pdf:native-edit-transition-failed",
  "pdf:native-reading-failed",
  "pdf:page-render",
  "pdf:ocr-page",
  "pdf:ocr-page-failed",
  "pdf:ocr-document",
  "pdf-save:plan-input",
  "pdf-save:prepare:done",
  "pdf-save:prepare:error",
  "pdf-save:verify:done",
  "pdf-save:verify:error",
  "pdf-editor:inspect",
  "pdf-editor:initial-stamp",
  "pdf-editor:layer-init",
  "pdf-editor:init",
  "pdf-editor:mode-switch",
  "pdf-editor:serialize:start",
  "pdf-editor:serialize:done",
  "pdf-editor:serialize",
  "pdf-editor:preview",
  "pdf-editor:reload",
  "pdf-editor:reload-required:memory-edits",
  "pdf-editor:reload-required:annotation-edits",
  "pdf-editor:reload-required:reload-failed",
  "pdf-editor:reload-retry",
  "pdf-bench-invoked",
  "pdf-bench-begin",
  "pdf-bench-baseline-ready",
  "pdf-bench-session-active",
  "pdf-bench-watchdog-fired",
  "pdf-bench-document-ready",
  "pdf-bench-first-canvas",
  "pdf-bench-finalize",
  "pdf-bench-autopilot-start",
  "pdf-bench-autopilot-result",
  "pdf-bench-autopilot-done",
] as const;

export type DiagnosticEvent = (typeof DIAGNOSTIC_EVENTS)[number];

// Mirrors settings.tsx STORE_KEY. diagnostics runs before the React settings context, so it reads
// the persisted settings directly (once) to honor the diagnostics toggle. Default ON (absent
// settings = on).
const SETTINGS_KEY = "vr.settings.v1";
const TAURI_LISTENER_TEARDOWN_RACE = "listeners[eventId].handlerId";

function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function enabledFromSettingsJson(raw: string | null): boolean {
  if (!raw) return true;
  try {
    return (JSON.parse(raw) as { diagnostics?: boolean }).diagnostics !== false;
  } catch {
    return true;
  }
}

function readEnabledFromSettings(): boolean {
  try {
    return enabledFromSettingsJson(localStorage.getItem(SETTINGS_KEY));
  } catch {
    return true;
  }
}

// Privacy toggle. Gates the recorder (via `setEnabled`) and forwarding to the native shared log.
// Kept current by `setDiagnosticsEnabled` (App mirrors the settings context into it) and by
// `storage` events from other windows.
let enabled = readEnabledFromSettings();

// Capacity (80), consecutive-event dedup (1000 ms), synchronous flush of load/error/rejection, and
// idle / pagehide / hidden flush are the component's fixed behavior.
const recorder = createDiagnosticsRecorder({
  storageKey: "vr.diagnostics",
  allowedEvents: DIAGNOSTIC_EVENTS,
  enabled,
});

// `name` carries the call site's static label ("open:start", "mdx:compile-error") so an exported
// bundle says WHAT happened rather than just how often. No `detail` field exists anywhere: it held
// file paths, document names, and hrefs, none of which belong in telemetry.
const sharedLogs = createLogsClient({
  invoke,
  allowedFields: ["kind", "surface", "duration_ms", "name"],
});

/**
 * Keep the cached privacy toggle in step with the settings context. The settings provider persists
 * localStorage & the SQLite mirror; App calls this whenever `settings.diagnostics` changes. Turning
 * it off flushes already-accepted entries (component behavior).
 */
export function setDiagnosticsEnabled(next: boolean) {
  enabled = next;
  recorder.setEnabled(next);
}

// `nativeLogged`: the entry already reaches the native log through the shared crash capture, so the
// renderer-event mirror below is skipped (no double record of one uncaught error).
function push(input: DiagnosticInput, nativeLogged = false) {
  // `record` is false when disabled or deduplicated (same kind+event within 1 s).
  const accepted = recorder.record(input);
  if (accepted && isTauri() && !nativeLogged) {
    const fields = { kind: input.kind, surface: "renderer", name: input.event };
    if (input.kind === "error" || input.kind === "rejection") {
      sharedLogs.error("renderer_error", fields);
    } else if (input.kind === "perf") {
      sharedLogs.info("renderer_perf", { ...fields, duration_ms: input.duration_ms ?? 0 });
    } else {
      sharedLogs.info(`renderer_${input.kind}`, fields);
    }
  }
  // Errors are always console-logged regardless of the toggle (a duplicate within the dedup window
  // is skipped while enabled).
  if ((input.kind === "error" || input.kind === "rejection") && (accepted || !enabled)) {
    console.error("[vr]", input.kind, input.event);
  }
}

/** Record a named lifecycle event (e.g. opening/rendering a document) for trace context. */
export function logEvent(event: DiagnosticEvent) {
  push({ kind: "event", event });
}

/**
 * Record a HANDLED failure — one the app recovered from, but that must stay visible.
 *
 * Use this, not logEvent, whenever a catch block swallows something a user would call a bug.
 * The label reaches telemetry as the `name` field, but an event still arrives as `renderer_event`
 * at INFO level, among hundreds of routine ones. "error" arrives as `renderer_error` at error
 * level, which is what someone reading an export actually filters on.
 * (2026-07-17: the legal gate's rejected window resize was logged as an event and stayed invisible
 * because nothing distinguished it; naming helps, but level is still what makes it findable.)
 */
export function logError(event: DiagnosticEvent) {
  push({ kind: "error", event });
}

/**
 * Record elapsed time for local pressure-test breadcrumbs.
 * @param startedAt Timestamp from performance.now(), not Date.now().
 */
export function logPerf(event: DiagnosticEvent, startedAt: number) {
  push({
    kind: "perf",
    event,
    duration_ms: Math.max(0, Math.round(performance.now() - startedAt)),
  });
}

export function getDiagnostics(): DiagnosticEntry[] {
  return recorder.snapshot();
}

/** Collect the durable Rust bundle in Tauri; retain the browser log for QA/dev fallback. */
export async function collectDiagnosticsText(): Promise<string> {
  if (!isTauri()) return recorder.text();
  const bundle = await sharedLogs.collectDiagnostics<unknown>();
  return JSON.stringify(bundle, null, 2);
}

/** Clear both the compatibility browser trail and the durable shared log. Rejects when the browser
 *  trail could not be erased, so callers never present a failed erase as success. */
export async function clearAllDiagnostics(): Promise<void> {
  const local = recorder.clear();
  if (isTauri()) await sharedLogs.clear();
  if (local === "failed") throw new Error("Local diagnostics could not be erased");
}

function isBenignTauriListenerTeardown(reason: unknown): boolean {
  return String(reason ?? "").includes(TAURI_LISTENER_TEARDOWN_RACE);
}

let installed = false;

export function installDiagnostics() {
  if (installed) return;
  installed = true;
  push({ kind: "load", event: "app:loaded" });
  window.addEventListener("error", (e) => {
    // Chromium/WebView2 report a ResizeObserver callback that resized observed content as an
    // error event. It is a benign layout notice, not an app fault, so record it as an event.
    if (/^ResizeObserver loop/.test(e.message ?? "")) {
      push({ kind: "event", event: "layout:resize-observer-loop" });
      // Benign: keep it out of the crash capture registered after this listener.
      e.stopImmediatePropagation();
      return;
    }
    push({ kind: "error", event: "window:error" }, isTauri());
  });
  window.addEventListener("unhandledrejection", (e) => {
    const reason = (e as PromiseRejectionEvent).reason;
    if (isBenignTauriListenerTeardown(reason)) {
      e.preventDefault();
      push({ kind: "event", event: "tauri:event-listener-teardown" });
      e.stopImmediatePropagation();
      return;
    }
    push({ kind: "rejection", event: "window:unhandledrejection" }, isTauri());
  });
  // Settings changed in another window (same-window writes don't fire `storage`; App covers those).
  window.addEventListener("storage", (e) => {
    if (e.key === SETTINGS_KEY || e.key === null) {
      setDiagnosticsEnabled(enabledFromSettingsJson(e.key === null ? null : e.newValue));
    }
  });
  // Registered after the two listeners above so their benign-case stopImmediatePropagation applies.
  // Reports reach the native log through `rightkit_crash_window_error`; it honors the diagnostics
  // toggle because the native log gate drops them while diagnostics are off.
  if (isTauri()) installWindowCrashCapture({ invoke });
  // Console helpers to dump / clear the trail after a restart.
  const w = window as unknown as {
    vrDiagnostics?: () => DiagnosticEntry[];
    vrDiagnosticsText?: () => string;
    vrClearDiagnostics?: () => void;
  };
  w.vrDiagnostics = getDiagnostics;
  w.vrDiagnosticsText = () => recorder.text();
  w.vrClearDiagnostics = () => {
    recorder.clear();
  };
}
