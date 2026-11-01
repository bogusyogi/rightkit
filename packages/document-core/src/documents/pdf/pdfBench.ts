import { isTauri } from "@rightkit/platform-ui";
import { invoke } from "@tauri-apps/api/core";
import { getResourceCensus, type ResourceCensus } from "../../app/resourceCensus";
import { isMac, isWindows } from "../../platform";
import { logEvent } from "../../state/diagnostics";
import {
  autopilotOnComplete,
  autopilotStatus,
  maybeAutoStartFromEnv,
  startAutopilot,
  stopAutopilot,
} from "./pdfBenchAutopilot";

export type PdfBenchLoadMode = "buffer" | "url-native" | "custom-range";

// The harness is dev-only. Every export starts with a literal `import.meta.env.DEV` check (not a
// helper call) so production builds fold each one to its no-op and the minifier drops the harness,
// its storage keys and the autopilot. Env values are read by explicit key for the same reason: a
// computed `import.meta.env[key]` makes Vite inline the whole env object.

const ENABLED_KEY = "vr.pdf.bench.enabled";
const MODE_KEY = "vr.pdf.bench.mode";
const HISTORY_KEY = "vr.pdf.bench.history.v1";
const MAX_HISTORY = 40;
const SETTLE_POLL_MS = 250;
const SETTLE_QUIET_MS = 1_000;
const SETTLE_MAX_MS = 6_000;
const WATCHDOG_MS = 30_000;
const FIXTURE_LABEL_KEY = "vr.pdf.bench.fixture.label";
const SOURCE_FILENAME_KEY = "vr.pdf.bench.fixture.filename";
const FIXTURE_BYTES_KEY = "vr.pdf.bench.fixture.bytes";
const FIXTURE_SHA256_KEY = "vr.pdf.bench.fixture.sha256";

export interface PdfBenchStreamStats {
  readonly enabled: boolean;
  readonly resource_bytes: number;
  readonly head_requests: number;
  readonly full_get_requests: number;
  readonly range_get_requests: number;
  readonly rejected_range_requests: number;
  readonly full_bytes_read: number;
  readonly range_bytes_read: number;
  readonly total_bytes_read: number;
  readonly max_response_bytes: number;
}

export interface PdfBenchProcessMemory {
  /** RSS for the app process only; this is not total webview memory. */
  readonly app_process_rss_bytes: number;
  /** RSS summed across the app process tree; this is not total webview memory. */
  readonly app_process_tree_rss_bytes: number;
  readonly process_count: number;
}

export interface PdfBenchRuntimeStats {
  readonly js_heap_used_bytes: number | null;
  readonly js_heap_total_bytes: number | null;
  readonly js_heap_limit_bytes: number | null;
  readonly longtask_count: number;
  readonly longtask_duration_ms: number;
}

export interface PdfBenchSnapshot {
  readonly at_ms: number;
  readonly stream: PdfBenchStreamStats | null;
  readonly process_memory: PdfBenchProcessMemory | null;
  readonly runtime: PdfBenchRuntimeStats;
  readonly resources: ResourceCensus;
}

export interface PdfBenchResult {
  readonly schema_version: 1;
  readonly mode: PdfBenchLoadMode;
  readonly run_id: string;
  readonly launch_kind: string | null;
  readonly run_index: number | null;
  readonly run_total: number | null;
  readonly started_at: string;
  readonly platform: string;
  readonly user_agent: string;
  readonly pdfjs_version: string;
  readonly document_ready_ms: number | null;
  readonly first_canvas_ms: number | null;
  readonly first_canvas_page: number | null;
  readonly baseline_memory: PdfBenchProcessMemory | null;
  readonly baseline_runtime: PdfBenchRuntimeStats;
  readonly first_canvas_snapshot: PdfBenchSnapshot | null;
  readonly late_page_target: number | null;
  readonly late_page_request_ms: number | null;
  readonly late_page_ms: number | null;
  readonly late_page_snapshot: PdfBenchSnapshot | null;
  readonly settled_snapshot: PdfBenchSnapshot | null;
  readonly fixture_label: string | null;
  readonly source_filename: string | null;
  readonly fixture_bytes: number | null;
  readonly fixture_sha256: string | null;
  readonly fixture_identity_status: "missing" | "pending" | "matched" | "mismatch";
  readonly error: string | null;
}

interface MutableSession {
  readonly url: string;
  readonly mode: PdfBenchLoadMode;
  readonly run_id: string;
  readonly launch_kind: string | null;
  readonly startedAt: number;
  readonly startedIso: string;
  readonly platform: string;
  readonly userAgent: string;
  readonly pdfjsVersion: string;
  readonly baselineMemory: PdfBenchProcessMemory | null;
  baselineRuntime: PdfBenchRuntimeStats;
  readonly fixtureLabel: string | null;
  readonly sourceFilename: string | null;
  readonly fixtureBytes: number | null;
  readonly fixtureSha256: string | null;
  fixtureIdentityStatus: "missing" | "pending" | "matched" | "mismatch";
  longTaskObserver: PerformanceObserver | null;
  longTaskCount: number;
  longTaskDurationMs: number;
  watchdogHandle: number | null;
  documentReadyMs: number | null;
  firstCanvasMs: number | null;
  firstCanvasPage: number | null;
  firstCanvasSnapshot: PdfBenchSnapshot | null;
  latePageTarget: number | null;
  latePagePending: boolean;
  latePageMs: number | null;
  latePageSnapshot: PdfBenchSnapshot | null;
  latePageRequestedAt: number | null;
  settledSnapshot: PdfBenchSnapshot | null;
  error: string | null;
  finalized: boolean;
}

let active: MutableSession | null = null;
let settleGeneration = 0;
let beginGeneration = 0;
let latePageNavigator: ((requested: number) => number | null) | null = null;

function benchEnabled(): boolean {
  if (typeof window === "undefined" || !import.meta.env.DEV) return false;
  try {
    return localStorage.getItem(ENABLED_KEY) === "1";
  } catch {
    return false;
  }
}

export function isPdfBenchEnabled(): boolean {
  return import.meta.env.DEV && benchEnabled();
}

function normalizeMode(value: string | null): PdfBenchLoadMode {
  return value === "url-native" || value === "custom-range" ? value : "buffer";
}

function isWindowsPdfRuntime(): boolean {
  return isWindows();
}

export function getPdfBenchLoadMode(): PdfBenchLoadMode {
  // Mac WKWebView corpus: custom range matched/surpassed full-fetch latency while avoiding the
  // initial whole-file read on large PDFs. Windows/WebView2 keeps the prior full-buffer default
  // until a recorded Windows corpus confirms custom range there. Benchmark mode may still
  // override this per localStorage for either platform.
  // Browser QA and web previews can use data/blob/http URLs that do not promise byte-range
  // semantics. Custom range is a native macOS transport choice, so keep non-Tauri runtimes on
  // the portable full-buffer path unless benchmark mode explicitly overrides it.
  if (!import.meta.env.DEV || !benchEnabled()) {
    return !isTauri() || isWindowsPdfRuntime() ? "buffer" : "custom-range";
  }
  try {
    return normalizeMode(localStorage.getItem(MODE_KEY));
  } catch {
    return isWindowsPdfRuntime() ? "buffer" : "custom-range";
  }
}

export function registerPdfBenchLatePageNavigator(
  navigator: ((requested: number) => number | null) | null,
): void {
  if (!import.meta.env.DEV) return;
  latePageNavigator = navigator;
}

function latePageTarget(): number | null {
  const raw = import.meta.env.VITE_PDF_BENCH_LATE_PAGE as string | undefined;
  if (!raw || !/^\d+$/.test(raw)) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value > 0 ? value : null;
}

async function safeInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!isTauri()) return null;
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    console.warn(`[pdf-bench] ${command} failed`, error);
    return null;
  }
}

async function streamStats(): Promise<PdfBenchStreamStats | null> {
  return safeInvoke<PdfBenchStreamStats>("pdf_bench_stream_stats");
}

async function processMemory(): Promise<PdfBenchProcessMemory | null> {
  const value = await safeInvoke<{
    app_process_rss_bytes: number;
    app_process_tree_rss_bytes: number;
    app_process_count: number;
  }>("pdf_bench_process_memory");
  return value
    ? {
        app_process_rss_bytes: value.app_process_rss_bytes,
        app_process_tree_rss_bytes: value.app_process_tree_rss_bytes,
        process_count: value.app_process_count,
      }
    : null;
}

async function activateMacBenchWindow(): Promise<void> {
  if (!isTauri() || !isMac()) return;
  const deadline = performance.now() + 2_000;
  do {
    await safeInvoke("pdf_bench_activate_foreground");
    await new Promise((resolve) => window.setTimeout(resolve, 100));
    if (document.visibilityState === "visible" && document.hasFocus()) return;
  } while (performance.now() < deadline);
}

function envOrStored(key: string, env: string | undefined): string | null {
  if (env) return env;
  try {
    return localStorage.getItem(key) || null;
  } catch {
    return null;
  }
}

function fixtureIdentity() {
  const bytesRaw = envOrStored(FIXTURE_BYTES_KEY, import.meta.env.VITE_PDF_BENCH_FIXTURE_BYTES);
  const bytes = bytesRaw && /^\d+$/.test(bytesRaw) ? Number(bytesRaw) : null;
  const sha = envOrStored(FIXTURE_SHA256_KEY, import.meta.env.VITE_PDF_BENCH_FIXTURE_SHA256);
  return {
    label: envOrStored(FIXTURE_LABEL_KEY, import.meta.env.VITE_PDF_BENCH_FIXTURE_LABEL),
    sourceFilename: envOrStored(
      SOURCE_FILENAME_KEY,
      import.meta.env.VITE_PDF_BENCH_SOURCE_FILENAME,
    ),
    bytes: bytes !== null && Number.isSafeInteger(bytes) && bytes > 0 ? bytes : null,
    sha256: sha && /^[0-9a-f]{64}$/i.test(sha) ? sha.toLowerCase() : null,
  };
}

function runtimeStats(session: MutableSession): PdfBenchRuntimeStats {
  const memory = (
    performance as Performance & {
      memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number };
    }
  ).memory;
  return {
    js_heap_used_bytes: memory?.usedJSHeapSize ?? null,
    js_heap_total_bytes: memory?.totalJSHeapSize ?? null,
    js_heap_limit_bytes: memory?.jsHeapSizeLimit ?? null,
    longtask_count: session.longTaskCount,
    longtask_duration_ms: session.longTaskDurationMs,
  };
}

function elapsed(session: MutableSession): number {
  return Math.max(0, performance.now() - session.startedAt);
}

function serializable(session: MutableSession): PdfBenchResult {
  const plan = autopilotStatus();
  return {
    schema_version: 1,
    mode: session.mode,
    run_id: session.run_id,
    launch_kind: session.launch_kind,
    run_index: plan && !plan.done ? plan.index + 1 : null,
    run_total: plan ? plan.modes.length * plan.repeats : null,
    started_at: session.startedIso,
    platform: session.platform,
    user_agent: session.userAgent,
    pdfjs_version: session.pdfjsVersion,
    document_ready_ms: session.documentReadyMs,
    first_canvas_ms: session.firstCanvasMs,
    first_canvas_page: session.firstCanvasPage,
    baseline_memory: session.baselineMemory,
    baseline_runtime: session.baselineRuntime,
    first_canvas_snapshot: session.firstCanvasSnapshot,
    late_page_target: session.latePageTarget,
    late_page_request_ms: session.latePageRequestedAt,
    late_page_ms: session.latePageMs,
    late_page_snapshot: session.latePageSnapshot,
    settled_snapshot: session.settledSnapshot,
    fixture_label: session.fixtureLabel,
    source_filename: session.sourceFilename,
    fixture_bytes: session.fixtureBytes,
    fixture_sha256: session.fixtureSha256,
    fixture_identity_status: session.fixtureIdentityStatus,
    error: session.error,
  };
}

function loadHistory(): PdfBenchResult[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as PdfBenchResult[]) : [];
  } catch {
    return [];
  }
}

function saveHistory(result: PdfBenchResult): void {
  try {
    const history = loadHistory();
    history.push(result);
    while (history.length > MAX_HISTORY) history.shift();
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
  } catch {
    // Dev-only benchmark history; never an application correctness dependency.
  }
}

async function capture(session: MutableSession): Promise<PdfBenchSnapshot> {
  const [stream, memory] = await Promise.all([streamStats(), processMemory()]);
  if (session.fixtureBytes !== null && stream?.resource_bytes) {
    session.fixtureIdentityStatus =
      stream.resource_bytes === session.fixtureBytes ? "matched" : "mismatch";
    if (session.fixtureIdentityStatus === "mismatch" && !session.error) {
      session.error = `fixture byte mismatch: expected ${session.fixtureBytes}, native reported ${stream.resource_bytes}`;
    }
  }
  return {
    at_ms: elapsed(session),
    stream,
    process_memory: memory,
    runtime: runtimeStats(session),
    resources: getResourceCensus(),
  };
}

async function finalize(session: MutableSession): Promise<void> {
  if (session.finalized) return;
  session.finalized = true;
  if (session.watchdogHandle !== null) window.clearTimeout(session.watchdogHandle);
  session.longTaskObserver?.disconnect();
  const result = serializable(session);
  logEvent("pdf-bench-finalize");
  saveHistory(result);
  let artifactPath: string | null = null;
  if (isTauri()) {
    try {
      artifactPath = await invoke<string>("pdf_bench_write_result", {
        runId: result.run_id,
        result,
      });
    } catch (error) {
      console.error("[pdf-bench] durable artifact write failed", error);
      return;
    }
  }
  if (artifactPath) console.info(`[pdf-bench] durable artifact ${artifactPath}`);
  await safeInvoke("pdf_bench_stop");
  console.info("[pdf-bench] completed", result);
  // Unattended matrix: record to the diagnostics log and advance to the next mode. No-op unless a
  // plan is active, so hand-driven console runs behave exactly as before.
  autopilotOnComplete();
}

async function settle(session: MutableSession, generation: number): Promise<void> {
  const settleStarted = performance.now();
  let lastSignature = "";
  let stableSince = performance.now();

  while (
    active === session &&
    generation === settleGeneration &&
    performance.now() - settleStarted < SETTLE_MAX_MS
  ) {
    await new Promise((resolve) => window.setTimeout(resolve, SETTLE_POLL_MS));
    const stats = await streamStats();
    const signature = stats
      ? `${stats.head_requests}:${stats.full_get_requests}:${stats.range_get_requests}:${stats.total_bytes_read}`
      : "no-native-stats";

    if (signature !== lastSignature) {
      lastSignature = signature;
      stableSince = performance.now();
      continue;
    }
    if (performance.now() - stableSince >= SETTLE_QUIET_MS) break;
  }

  if (active !== session || generation !== settleGeneration) return;
  session.settledSnapshot = await capture(session);
  await finalize(session);
}

export async function beginPdfBench(
  url: string,
  pdfjsVersion: string,
  mode = getPdfBenchLoadMode(),
): Promise<void> {
  if (!import.meta.env.DEV) return;
  const enabled = benchEnabled();
  if (!enabled) return;

  logEvent("pdf-bench-invoked");
  logEvent("pdf-bench-begin");

  const generation = ++beginGeneration;
  settleGeneration++;
  await activateMacBenchWindow();
  await safeInvoke("pdf_bench_reset");
  const baselineMemory = await processMemory();
  if (generation !== beginGeneration) return;
  logEvent("pdf-bench-baseline-ready");
  const identity = fixtureIdentity();
  const baselineRuntime: PdfBenchRuntimeStats = {
    js_heap_used_bytes: null,
    js_heap_total_bytes: null,
    js_heap_limit_bytes: null,
    longtask_count: 0,
    longtask_duration_ms: 0,
  };

  const session: MutableSession = {
    url,
    mode,
    run_id: String(import.meta.env.VITE_PDF_BENCH_RUN_ID ?? `run-${Date.now()}`),
    launch_kind: (import.meta.env.VITE_PDF_BENCH_LAUNCH_KIND as string | undefined) ?? null,
    startedAt: performance.now(),
    startedIso: new Date().toISOString(),
    platform: navigator.platform,
    userAgent: navigator.userAgent,
    pdfjsVersion,
    baselineMemory,
    baselineRuntime,
    fixtureLabel: identity.label,
    sourceFilename: identity.sourceFilename,
    fixtureBytes: identity.bytes,
    fixtureSha256: identity.sha256,
    fixtureIdentityStatus: identity.bytes === null ? "missing" : "pending",
    longTaskObserver: null,
    longTaskCount: 0,
    longTaskDurationMs: 0,
    watchdogHandle: null,
    documentReadyMs: null,
    firstCanvasMs: null,
    firstCanvasPage: null,
    firstCanvasSnapshot: null,
    latePageTarget: latePageTarget(),
    latePagePending: false,
    latePageMs: null,
    latePageSnapshot: null,
    latePageRequestedAt: null,
    settledSnapshot: null,
    error: null,
    finalized: false,
  };
  let observer: PerformanceObserver | null = null;
  try {
    if (typeof PerformanceObserver !== "undefined") {
      observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          session.longTaskCount += 1;
          session.longTaskDurationMs += entry.duration;
        }
      });
    }
    observer?.observe({ type: "longtask", buffered: true });
  } catch {
    // Long-task timing is optional in WebView.
    observer = null;
  }
  session.longTaskObserver = observer;
  session.baselineRuntime = runtimeStats(session);

  active = session;
  logEvent("pdf-bench-session-active");
  session.watchdogHandle = window.setTimeout(() => {
    if (active !== session || session.finalized) return;
    logEvent("pdf-bench-watchdog-fired");
    session.error =
      session.documentReadyMs === null
        ? "watchdog: document-ready timeout"
        : session.latePagePending
          ? "watchdog: late-page timeout"
          : "watchdog: first-canvas timeout";
    void capture(session).then(async (snapshot) => {
      if (active !== session) return;
      session.settledSnapshot = snapshot;
      await finalize(session);
    });
  }, WATCHDOG_MS);

  console.info(`[pdf-bench] started mode=${mode}`);
}

export function markPdfBenchDocumentReady(url: string): void {
  if (!import.meta.env.DEV) return;
  const session = active;
  if (!session || session.url !== url || session.documentReadyMs !== null) return;
  session.documentReadyMs = elapsed(session);
  logEvent("pdf-bench-document-ready");
}

export function markPdfBenchFirstCanvasPaint(url: string | undefined, page: number): void {
  if (!import.meta.env.DEV) return;
  const session = active;
  if (!url || !session || session.url !== url) return;
  if (session.firstCanvasMs !== null) {
    markLatePagePaint(session, page);
    return;
  }

  // Record latency before memory/stat snapshots add any benchmark work.
  session.firstCanvasMs = elapsed(session);
  session.firstCanvasPage = page;
  logEvent("pdf-bench-first-canvas");

  void capture(session).then((snapshot) => {
    if (active === session) session.firstCanvasSnapshot = snapshot;
    if (active !== session) return;

    const target = session.latePageTarget;
    if (target === null || latePageNavigator === null) {
      const generation = ++settleGeneration;
      void settle(session, generation);
      return;
    }

    if (target === page) {
      session.latePageRequestedAt = session.firstCanvasMs;
      session.latePageMs = 0;
      session.latePageSnapshot = snapshot;
      const generation = ++settleGeneration;
      void settle(session, generation);
      return;
    }

    session.latePagePending = true;
    session.latePageRequestedAt = elapsed(session);
    const actual = latePageNavigator(target);
    if (actual === null || !Number.isSafeInteger(actual) || actual <= 0) {
      session.latePagePending = false;
      const generation = ++settleGeneration;
      void settle(session, generation);
      return;
    }
    session.latePageTarget = actual;
    if (actual === page) {
      session.latePagePending = false;
      session.latePageMs = 0;
      session.latePageSnapshot = snapshot;
      const generation = ++settleGeneration;
      void settle(session, generation);
    }
  });
}

function markLatePagePaint(session: MutableSession, page: number): void {
  if (!session.latePagePending || session.latePageTarget === null || session.latePageMs !== null)
    return;
  if (page !== session.latePageTarget) return;
  const arrivedAt = elapsed(session);
  session.latePageMs = Math.max(0, arrivedAt - (session.latePageRequestedAt ?? arrivedAt));
  void capture(session).then((snapshot) => {
    if (active !== session) return;
    session.latePageSnapshot = snapshot;
    session.latePagePending = false;
    const generation = ++settleGeneration;
    void settle(session, generation);
  });
}

export function markPdfBenchError(url: string, error: unknown): void {
  if (!import.meta.env.DEV) return;
  const session = active;
  if (!session || session.url !== url || session.error) return;
  session.error = String((error as { message?: unknown })?.message ?? error);
  void capture(session).then(async (snapshot) => {
    if (active !== session) return;
    session.settledSnapshot = snapshot;
    await finalize(session);
  });
}

async function currentReport(): Promise<PdfBenchResult | null> {
  const session = active;
  if (!session) return null;
  if (!session.settledSnapshot) session.settledSnapshot = await capture(session);
  return serializable(session);
}

function downloadJson(filename: string, value: unknown): void {
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
}

function installConsoleApi(): void {
  if (typeof window === "undefined" || !import.meta.env.DEV) return;

  const api = {
    status: () => ({
      enabled: benchEnabled(),
      mode: getPdfBenchLoadMode(),
      active: active ? serializable(active) : null,
      history_count: loadHistory().length,
    }),
    enable: (mode: PdfBenchLoadMode = "buffer", reload = true) => {
      localStorage.setItem(ENABLED_KEY, "1");
      localStorage.setItem(MODE_KEY, normalizeMode(mode));
      if (reload) location.reload();
    },
    disable: (reload = false) => {
      localStorage.removeItem(ENABLED_KEY);
      void safeInvoke("pdf_bench_stop");
      if (reload) location.reload();
    },
    setMode: (mode: PdfBenchLoadMode, reload = true) => {
      localStorage.setItem(ENABLED_KEY, "1");
      localStorage.setItem(MODE_KEY, normalizeMode(mode));
      if (reload) location.reload();
    },
    report: currentReport,
    history: loadHistory,
    clearHistory: () => localStorage.removeItem(HISTORY_KEY),
    download: async () => {
      const report = await currentReport();
      const payload = {
        schema_version: 1,
        exported_at: new Date().toISOString(),
        history: loadHistory(),
        active: report,
      };
      downloadJson(`viewright-pdf-bench-${Date.now()}.json`, payload);
      return payload;
    },
    // Unattended matrix (modes x repeats), results written to the diagnostics log.
    autopilot: (label?: string, repeats?: number) =>
      startAutopilot(label ?? "manual", undefined, repeats),
    autopilotStatus,
    stopAutopilot,
  };

  Object.defineProperty(window, "vrPdfBench", {
    configurable: true,
    enumerable: false,
    value: api,
  });

  // Env-driven start so the matrix needs no console at all:
  //   VITE_PDF_BENCH_AUTOPILOT=1 pnpm dev:tauri <file.pdf>
  maybeAutoStartFromEnv();
}

declare global {
  interface Window {
    vrPdfBench?: {
      status(): unknown;
      enable(mode?: PdfBenchLoadMode, reload?: boolean): void;
      disable(reload?: boolean): void;
      setMode(mode: PdfBenchLoadMode, reload?: boolean): void;
      report(): Promise<PdfBenchResult | null>;
      history(): PdfBenchResult[];
      clearHistory(): void;
      download(): Promise<unknown>;
      autopilot(label?: string, repeats?: number): void;
      autopilotStatus(): unknown;
      stopAutopilot(): void;
    };
  }
}

installConsoleApi();
