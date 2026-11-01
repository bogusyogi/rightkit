import { isWindows } from "@rightkit/document-core/platform";
import { getResourceCensus, type DocumentFormat, type ResourceCensus } from "@rightkit/document-core/app/resourceCensus";

type ResourceMatrixFormat = "shell" | "txt" | "md" | "html" | "docx" | "image" | "pdf";
type ResourceMatrixScenario = "A" | "B" | "E";
const RESOURCE_MATRIX_FORMATS = new Set<ResourceMatrixFormat>([
  "shell",
  "txt",
  "md",
  "html",
  "docx",
  "image",
  "pdf",
]);
const MATRIX_POLL_MS = 100;
const MATRIX_QUIET_MS = 1_000;
const MATRIX_TIMEOUT_MS = 45_000;
const HEAVY_RESOURCE_FORMATS = new Set<ResourceMatrixFormat>(["html", "docx", "image", "pdf"]);

type ResourceMatrixPlanDocument = {
  readonly format: Exclude<ResourceMatrixFormat, "shell">;
  readonly path: string;
  readonly fixture: {
    readonly source_filename: string;
    readonly bytes: number;
    readonly sha256: string;
  };
};

type ResourceMatrixPlan =
  | {
      readonly scenario: "C";
      readonly a: ResourceMatrixPlanDocument;
      readonly b: ResourceMatrixPlanDocument;
    }
  | {
      readonly scenario: "D";
      readonly tab_count: 10 | 50;
      readonly dirty_docx_path: string;
      readonly fixtures: readonly ResourceMatrixPlanDocument[];
    };

function matrixTimeoutMs(): number {
  const requested = Number(import.meta.env.VITE_RESOURCE_MATRIX_TIMEOUT_MS);
  return Number.isSafeInteger(requested) && requested >= 5_000 && requested <= 180_000
    ? requested
    : MATRIX_TIMEOUT_MS;
}

declare global {
  interface Window {
    /**
     * Development/QA-only read surface for the cross-format resource matrix. It deliberately
     * exposes a snapshot function, not mutable registry state or app controls.
     */
    __viewrightResourceCensus?: () => ResourceCensus;
    /** Development-only one-shot matrix probe status, for native QA inspection. */
    __viewrightResourceMatrixStatus?: () => {
      readonly state: string;
      readonly error: string | null;
    };
  }
}

/** Install only in dev or an explicitly-built browser-QA artifact; production has no global. */
export function installResourceCensusDevHook(): void {
  if (typeof window === "undefined") return;
  if (!import.meta.env.DEV && import.meta.env.VITE_RIGHTKIT_BROWSER_QA !== "1") return;
  window.__viewrightResourceCensus = getResourceCensus;
  installResourceMatrixAutopilot();
}

function resourceMatrixFormat(): ResourceMatrixFormat | null {
  const raw = import.meta.env.VITE_RESOURCE_MATRIX_FORMAT;
  return RESOURCE_MATRIX_FORMATS.has(raw as ResourceMatrixFormat)
    ? (raw as ResourceMatrixFormat)
    : null;
}

function isPlanDocument(value: unknown): value is ResourceMatrixPlanDocument {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<ResourceMatrixPlanDocument>;
  const format = (value as { format?: string }).format;
  return (
    typeof candidate.path === "string" &&
    format !== "shell" &&
    RESOURCE_MATRIX_FORMATS.has(format as ResourceMatrixFormat) &&
    !!candidate.fixture &&
    typeof candidate.fixture.source_filename === "string" &&
    Number.isSafeInteger(candidate.fixture.bytes) &&
    candidate.fixture.bytes > 0 &&
    typeof candidate.fixture.sha256 === "string" &&
    /^[0-9a-f]{64}$/i.test(candidate.fixture.sha256)
  );
}

/** Parse only an explicit native QA plan; malformed input leaves production/dev startup untouched. */
export function parseResourceMatrixPlan(raw: string | undefined): ResourceMatrixPlan | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<ResourceMatrixPlan>;
    if (value.scenario === "C" && isPlanDocument(value.a) && isPlanDocument(value.b)) {
      if (
        value.a.path !== value.b.path &&
        value.a.format !== value.b.format &&
        HEAVY_RESOURCE_FORMATS.has(value.a.format) &&
        HEAVY_RESOURCE_FORMATS.has(value.b.format)
      )
        return { scenario: "C", a: value.a, b: value.b };
    }
    if (
      value.scenario === "D" &&
      (value.tab_count === 10 || value.tab_count === 50) &&
      typeof value.dirty_docx_path === "string" &&
      Array.isArray(value.fixtures) &&
      value.fixtures.length === value.tab_count &&
      value.fixtures.every(isPlanDocument) &&
      value.fixtures.some(
        (fixture) => fixture.path === value.dirty_docx_path && fixture.format === "docx",
      )
    )
      return {
        scenario: "D",
        tab_count: value.tab_count,
        dirty_docx_path: value.dirty_docx_path,
        fixtures: value.fixtures,
      };
  } catch {
    // QA configuration is external input. A malformed plan must never change normal app startup.
  }
  return null;
}

function resourceMatrixScenario(format: ResourceMatrixFormat): ResourceMatrixScenario | null {
  if (format === "shell") return "A";
  const raw = import.meta.env.VITE_RESOURCE_MATRIX_SCENARIO;
  return raw === "B" || raw === "E" ? raw : null;
}

function resourceMatrixRuntime() {
  const memory = (
    performance as Performance & {
      memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number };
    }
  ).memory;
  return {
    js_heap_used_bytes: memory?.usedJSHeapSize ?? null,
    js_heap_total_bytes: memory?.totalJSHeapSize ?? null,
    js_heap_limit_bytes: memory?.jsHeapSizeLimit ?? null,
  };
}

function resourceMatrixTransfer() {
  const entries = performance.getEntriesByType("resource") as PerformanceResourceTiming[];
  return {
    requests: entries.length,
    transfer_bytes: entries.reduce((total, entry) => total + (entry.transferSize || 0), 0),
    decoded_body_bytes: entries.reduce((total, entry) => total + (entry.decodedBodySize || 0), 0),
  };
}

export function resourceMatrixReady(format: ResourceMatrixFormat, census: ResourceCensus): boolean {
  if (format === "shell") return census.heavy === 0;
  if (format === "pdf") {
    return (
      (census.byKind["pdf-page-canvas"] ?? 0) > 0 || (census.byKind["pdf-native-reader"] ?? 0) > 0
    );
  }
  return (census.byFormat[format]?.live ?? 0) > 0;
}

async function resourceMatrixProcessMemory() {
  if (!("__TAURI_INTERNALS__" in window)) return null;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<{
      app_process_rss_bytes: number;
      app_process_tree_rss_bytes: number;
      app_process_count: number;
    } | null>("pdf_bench_process_memory");
  } catch {
    return null;
  }
}

async function writeResourceMatrixResult(runId: string, result: unknown): Promise<void> {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("pdf_bench_write_result", { runId, result });
}

function platform(): "macos" | "windows" {
  return isWindows() ? "windows" : "macos";
}

type PaneState = {
  readonly pane_state: string | null;
  readonly router_state: string | null;
} | null;

function paneStates(paths: readonly string[]): Record<string, PaneState> {
  const panes = [...document.querySelectorAll<HTMLElement>("[data-vr-document-path]")];
  return Object.fromEntries(
    paths.map((path) => {
      const pane = panes.find((candidate) => candidate.dataset.vrDocumentPath === path);
      return [
        path,
        pane
          ? {
              pane_state: pane.dataset.vrPaneState ?? null,
              router_state: pane.dataset.vrRouterState ?? null,
            }
          : null,
      ];
    }),
  );
}

function clickTab(path: string): boolean {
  const tab = [...document.querySelectorAll<HTMLElement>("[data-vr-tab-path]")].find(
    (candidate) => candidate.dataset.vrTabPath === path,
  );
  if (!tab) return false;
  // Expanded rows own activation on their container but expose a real title button; collapsed
  // rows put the data attribute on that button. Clicking actual control matches user path on both.
  const control = tab.matches("button") ? tab : tab.querySelector<HTMLButtonElement>("button");
  (control ?? tab).click();
  return true;
}

async function waitForTab(path: string, deadline: number): Promise<void> {
  while (performance.now() < deadline) {
    if (
      [...document.querySelectorAll<HTMLElement>("[data-vr-tab-path]")].some(
        (candidate) => candidate.dataset.vrTabPath === path,
      )
    )
      return;
    await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
  }
  throw new Error(`timed out waiting for startup tab: ${path}`);
}

function activePath(): string | null {
  return (
    [...document.querySelectorAll<HTMLElement>("[data-vr-document-path]")].find(
      (candidate) => candidate.dataset.vrPaneState === "active",
    )?.dataset.vrDocumentPath ?? null
  );
}

async function waitForStablePath(
  format: ResourceMatrixFormat,
  path: string,
  deadline: number,
): Promise<{ census: ResourceCensus; readyAt: number }> {
  let quietSince: number | null = null;
  let lastSignature = "";
  let lastCensus = getResourceCensus();
  let lastActivationAttempt = 0;
  while (performance.now() < deadline) {
    if (activePath() !== path && performance.now() - lastActivationAttempt >= 500) {
      clickTab(path);
      lastActivationAttempt = performance.now();
    }
    const current = getResourceCensus();
    lastCensus = current;
    if (activePath() === path && resourceMatrixReady(format, current)) {
      // Readiness belongs to active target, not unrelated background work. A paint-warm PDF may
      // finish another canvas while DOCX activates; that is useful Scenario C evidence, not reason
      // to prevent DOCX from ever reaching useful-ready.
      const signature = JSON.stringify({
        target: current.byFormat[format as DocumentFormat] ?? null,
        pane: paneStates([path])[path],
      });
      if (signature !== lastSignature) {
        lastSignature = signature;
        quietSince = performance.now();
      }
      if (quietSince !== null && performance.now() - quietSince >= MATRIX_QUIET_MS)
        return { census: current, readyAt: performance.now() };
    } else {
      quietSince = null;
      lastSignature = "";
    }
    await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
  }
  throw new Error(
    `timed out waiting for ${format} at ${path}; active=${activePath() ?? "none"}; target=${JSON.stringify(lastCensus.byFormat[format as DocumentFormat] ?? null)}; pane=${JSON.stringify(paneStates([path])[path])}`,
  );
}

function dirtyDocxViaEditor(): boolean {
  const editor = document.querySelector<HTMLElement>(
    ".paged-editor__hidden-pm .ProseMirror[contenteditable='true']",
  );
  if (!editor || typeof document.execCommand !== "function") return false;
  editor.focus();
  // This goes through EigenPal's real ProseMirror input pipeline. Do not set textContent or mutate
  // census state: Scenario D is valid only when DocxPane's actual onChange marks it dirty.
  return document.execCommand("insertText", false, "•");
}

function closeShortcut(): void {
  const target =
    document.activeElement instanceof HTMLElement ? document.activeElement : document.body;
  target.dispatchEvent(
    new KeyboardEvent("keydown", {
      key: "w",
      code: "KeyW",
      ctrlKey: isWindows(),
      metaKey: !isWindows(),
      bubbles: true,
      cancelable: true,
    }),
  );
}

function dismissDontSave(): boolean {
  const button = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
    (candidate) => candidate.textContent?.trim().toLowerCase() === "don't save",
  );
  if (!button) return false;
  button.click();
  return true;
}

async function closeAllTabs(
  deadline: number,
): Promise<{ afterClose: ResourceCensus; released: boolean }> {
  const before = getResourceCensus();
  let priorCount = document.querySelectorAll("[data-vr-tab-path]").length;
  while (priorCount > 0 && performance.now() < deadline) {
    const countBeforeClose = priorCount;
    closeShortcut();
    const perCloseDeadline = Math.min(deadline, performance.now() + 4_000);
    while (performance.now() < perCloseDeadline) {
      dismissDontSave();
      const currentCount = document.querySelectorAll("[data-vr-tab-path]").length;
      if (currentCount < countBeforeClose) {
        priorCount = currentCount;
        break;
      }
      await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
    }
    if (priorCount >= countBeforeClose) break;
  }
  if (priorCount !== 0) throw new Error("timed out closing stress tabs");
  const quietUntil = performance.now() + MATRIX_QUIET_MS;
  while (performance.now() < quietUntil)
    await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
  const afterClose = getResourceCensus();
  return {
    afterClose,
    released: afterClose.totals.released > before.totals.released,
  };
}

async function runResourceMatrixPlan(
  plan: ResourceMatrixPlan,
  runId: string,
  baseline: ResourceCensus,
  startedAt: number,
): Promise<unknown> {
  const deadline = startedAt + matrixTimeoutMs();
  if (plan.scenario === "C") {
    // Cold-open routing activates each startup argument in order. Do not begin C until both tabs
    // exist, otherwise startup's final activation can overwrite our first A click with B.
    await Promise.all([waitForTab(plan.a.path, deadline), waitForTab(plan.b.path, deadline)]);
    const visits: Array<{
      phase: "a-first" | "b" | "a-return";
      census: ResourceCensus;
      panes: ReturnType<typeof paneStates>;
    }> = [];
    for (const [phase, item] of [
      ["a-first", plan.a],
      ["b", plan.b],
      ["a-return", plan.a],
    ] as const) {
      if (!clickTab(item.path)) throw new Error(`tab not found for ${phase}: ${item.path}`);
      const ready = await waitForStablePath(item.format, item.path, deadline);
      visits.push({
        phase,
        census: ready.census,
        panes: paneStates([plan.a.path, plan.b.path]),
      });
    }
    const final = visits[visits.length - 1];
    if (!final) throw new Error("switch plan did not collect final A visit");
    const finalPanes = final.panes;
    const bIsOnlyWarm =
      finalPanes[plan.b.path]?.pane_state === "warm" &&
      Object.values(finalPanes).filter((pane) => pane?.pane_state === "warm").length === 1;
    const perFormatMax = Object.fromEntries(
      [plan.a.format, plan.b.format].map((format) => [
        format,
        Math.max(
          ...visits.map((visit) => visit.census.byFormat[format as DocumentFormat]?.live ?? 0),
        ),
      ]),
    );
    return {
      schema_version: 1,
      scenario: "C",
      platform: platform(),
      format: plan.a.format,
      sample_id: runId,
      method: "native-resource-matrix-autopilot",
      fixture: plan.a.fixture,
      useful_paint_ms: Math.max(0, performance.now() - startedAt),
      peak_observed_ms: Math.max(0, performance.now() - startedAt),
      resources: { before: baseline, steady: final.census, after_close: null },
      runtime: resourceMatrixRuntime(),
      process_memory: await resourceMatrixProcessMemory(),
      transfer: resourceMatrixTransfer(),
      close: { attempted: false },
      tab_count: null,
      dirty_formats: [],
      switch: {
        a: plan.a,
        b: plan.b,
        visits,
        assertions: {
          previous_pane_only_paint_warm: bIsOnlyWarm,
          max_live_resources_per_format: perFormatMax,
        },
      },
      error: bIsOnlyWarm ? null : "A→B→A did not leave only B paint-warm on A return",
    };
  }

  while (performance.now() < deadline) {
    if (document.querySelectorAll("[data-vr-tab-path]").length >= plan.tab_count) break;
    await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
  }
  if (document.querySelectorAll("[data-vr-tab-path]").length < plan.tab_count)
    throw new Error(`timed out opening ${plan.tab_count} stress tabs`);
  if (!clickTab(plan.dirty_docx_path)) throw new Error("dirty DOCX tab was not found");
  const ready = await waitForStablePath("docx", plan.dirty_docx_path, deadline);
  if (!dirtyDocxViaEditor()) throw new Error("could not enter text through the real DOCX editor");
  let dirtyCensus = ready.census;
  while (performance.now() < deadline) {
    dirtyCensus = getResourceCensus();
    if ((dirtyCensus.byFormat.docx?.pinnedDirty ?? 0) > 0) break;
    await new Promise<void>((resolveWait) => window.setTimeout(resolveWait, MATRIX_POLL_MS));
  }
  if ((dirtyCensus.byFormat.docx?.pinnedDirty ?? 0) === 0)
    throw new Error("DOCX input did not produce a pinned dirty resource");
  const closed = await closeAllTabs(deadline);
  return {
    schema_version: 1,
    scenario: "D",
    platform: platform(),
    format: null,
    sample_id: runId,
    method: "native-resource-matrix-autopilot",
    fixture: null,
    useful_paint_ms: Math.max(0, ready.readyAt - startedAt),
    peak_observed_ms: Math.max(0, performance.now() - startedAt),
    resources: { before: baseline, steady: dirtyCensus, after_close: closed.afterClose },
    runtime: resourceMatrixRuntime(),
    process_memory: await resourceMatrixProcessMemory(),
    transfer: resourceMatrixTransfer(),
    close: { attempted: true },
    tab_count: plan.tab_count,
    dirty_formats: ["docx"],
    not_applicable: { pdf: "read-only" },
    stress: {
      fixtures: plan.fixtures,
      dirty_docx_path: plan.dirty_docx_path,
      close_released_resources: closed.released,
    },
    error: closed.released ? null : "closing stress tabs did not release a counted resource",
  };
}

/**
 * Native first-open collector. It is explicitly opt-in, uses existing debug-only PDF benchmark
 * commands solely as an append-only local artifact sink, & leaves all production behavior alone.
 */
function installResourceMatrixAutopilot(): void {
  if (import.meta.env.VITE_RESOURCE_MATRIX_AUTOPILOT !== "1") return;
  const plan = parseResourceMatrixPlan(import.meta.env.VITE_RESOURCE_MATRIX_PLAN);
  const planRunId = import.meta.env.VITE_RESOURCE_MATRIX_RUN_ID;
  if (plan && planRunId && "__TAURI_INTERNALS__" in window) {
    const startedAt = performance.now();
    const baseline = getResourceCensus();
    const state = { value: "running", error: null as string | null };
    window.__viewrightResourceMatrixStatus = () => ({ state: state.value, error: state.error });
    void runResourceMatrixPlan(plan, planRunId, baseline, startedAt)
      .then(async (result) => {
        state.value = "writing";
        await writeResourceMatrixResult(planRunId, result);
        state.value = "written";
      })
      .catch(async (error) => {
        state.value = "failed";
        state.error = String(error);
        void writeResourceMatrixResult(planRunId, {
          schema_version: 1,
          scenario: plan.scenario,
          platform: platform(),
          format: plan.scenario === "C" ? plan.a.format : null,
          sample_id: planRunId,
          method: "native-resource-matrix-autopilot",
          fixture: plan.scenario === "C" ? plan.a.fixture : null,
          useful_paint_ms: null,
          peak_observed_ms: Math.max(0, performance.now() - startedAt),
          resources: { before: baseline, steady: getResourceCensus(), after_close: null },
          runtime: resourceMatrixRuntime(),
          process_memory: await resourceMatrixProcessMemory(),
          transfer: resourceMatrixTransfer(),
          close: { attempted: false },
          tab_count: plan.scenario === "D" ? plan.tab_count : null,
          dirty_formats: [],
          ...(plan.scenario === "D" ? { not_applicable: { pdf: "read-only" } } : {}),
          error: state.error,
        });
      });
    return;
  }
  const format = resourceMatrixFormat();
  const runId = import.meta.env.VITE_RESOURCE_MATRIX_RUN_ID;
  const scenario = format ? resourceMatrixScenario(format) : null;
  if (!format || !scenario || !runId || !("__TAURI_INTERNALS__" in window)) return;
  const startedAt = performance.now();
  const baseline = getResourceCensus();
  const state = { value: "waiting", error: null as string | null };
  window.__viewrightResourceMatrixStatus = () => ({ state: state.value, error: state.error });
  let quietSince: number | null = null;
  let lastSignature = "";
  const deadline = startedAt + matrixTimeoutMs();

  const complete = async (
    error: string | null,
    steady: ResourceCensus,
    afterClose: ResourceCensus | null,
    usefulReadyAt: number,
  ) => {
    if (state.value === "writing" || state.value === "written" || state.value === "failed") return;
    state.value = "writing";
    state.error = error;
    const result = {
      schema_version: 1,
      scenario,
      platform: isWindows() ? "windows" : "macos",
      format: format === "shell" ? null : format,
      sample_id: runId,
      method: "native-resource-matrix-autopilot",
      fixture:
        format === "shell"
          ? null
          : {
              source_filename: import.meta.env.VITE_RESOURCE_MATRIX_FIXTURE_FILENAME ?? null,
              bytes: Number(import.meta.env.VITE_RESOURCE_MATRIX_FIXTURE_BYTES) || null,
              sha256: import.meta.env.VITE_RESOURCE_MATRIX_FIXTURE_SHA256 ?? null,
            },
      useful_paint_ms: error ? null : Math.max(0, usefulReadyAt - startedAt),
      peak_observed_ms: Math.max(0, performance.now() - startedAt),
      resources: { before: baseline, steady, after_close: afterClose },
      runtime: resourceMatrixRuntime(),
      process_memory: await resourceMatrixProcessMemory(),
      transfer: resourceMatrixTransfer(),
      close: { attempted: scenario === "E" },
      tab_count: null,
      dirty_formats: [],
      error,
    };
    try {
      await writeResourceMatrixResult(runId, result);
      state.value = "written";
    } catch (writeError) {
      state.value = "failed";
      state.error = `artifact write failed: ${String(writeError)}`;
    }
  };

  const closeAndComplete = (steady: ResourceCensus, usefulReadyAt: number) => {
    state.value = "closing";
    window.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "w",
        code: "KeyW",
        ctrlKey: isWindows(),
        metaKey: !isWindows(),
        bubbles: true,
        cancelable: true,
      }),
    );
    const pollClose = () => {
      const current = getResourceCensus();
      if (format !== "shell" && (current.byFormat[format]?.live ?? 0) === 0) {
        void complete(null, steady, current, usefulReadyAt);
        return;
      }
      if (performance.now() >= deadline) {
        void complete(
          `timed out waiting for ${format} close release`,
          steady,
          current,
          usefulReadyAt,
        );
        return;
      }
      window.setTimeout(pollClose, MATRIX_POLL_MS);
    };
    window.setTimeout(pollClose, MATRIX_POLL_MS);
  };

  const poll = () => {
    if (state.value !== "waiting") return;
    const now = performance.now();
    const current = getResourceCensus();
    if (!resourceMatrixReady(format, current)) {
      if (now >= deadline)
        void complete(`timed out waiting for ${format} useful readiness`, current, null, now);
      else window.setTimeout(poll, MATRIX_POLL_MS);
      return;
    }
    const signature = JSON.stringify(current);
    if (signature !== lastSignature) {
      lastSignature = signature;
      quietSince = now;
    }
    if (quietSince !== null && now - quietSince >= MATRIX_QUIET_MS) {
      if (scenario === "E") closeAndComplete(current, now);
      else void complete(null, current, null, now);
      return;
    }
    window.setTimeout(poll, MATRIX_POLL_MS);
  };
  window.setTimeout(poll, MATRIX_POLL_MS);
}
