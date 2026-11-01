/**
 * VR-P0-1 benchmark autopilot (dev-only, temporary instrumentation).
 *
 * The PDF loading benchmark has to run in the real WKWebView/WebView2 webview, whose DevTools
 * console is GUI-only — so driving it by hand does not scale to "3 repeats x 3 modes x N fixtures,
 * on two operating systems". This module runs the whole matrix unattended:
 *
 *   1. a plan (modes x repeats) is stored in localStorage;
 *   2. each app load runs whichever mode the plan currently points at;
 *   3. on completion the full result is appended to native PDF benchmark JSONL;
 *      a content-free diagnostics breadcrumb remains for quick status checks;
 *   4. the plan advances and the app reloads, until the matrix is done.
 *
 * Repeats are INTERLEAVED (round-robin across modes) rather than grouped, so a thermal or
 * page-cache drift over the run cannot systematically favour whichever mode ran first.
 *
 * Delete this file together with the rest of the benchmark harness once the study is decided.
 */

import { logEvent } from "../../state/diagnostics";
import type { PdfBenchLoadMode } from "./pdfBench";

const PLAN_KEY = "vr.pdf.bench.autopilot.v1";
const ENABLED_KEY = "vr.pdf.bench.enabled";
const MODE_KEY = "vr.pdf.bench.mode";

const DEFAULT_MODES: PdfBenchLoadMode[] = ["buffer", "url-native", "custom-range"];
const DEFAULT_REPEATS = 3;

interface AutopilotPlan {
  /** Native artifact identity. Distinguishes separate launcher processes using the same fixture. */
  readonly runId: string;
  readonly modes: PdfBenchLoadMode[];
  readonly repeats: number;
  /** Flattened run index: run n uses modes[n % modes.length]. */
  index: number;
  readonly label: string;
  done: boolean;
}

function dev(): boolean {
  // Vitest runs with MODE=test; retaining this explicit test-only gate keeps benchmark controls
  // unavailable in production builds while allowing orchestration behavior to stay covered.
  return (
    typeof window !== "undefined" &&
    (import.meta.env.DEV ||
      import.meta.env.MODE === "test" ||
      (typeof process !== "undefined" && process.env.NODE_ENV === "test"))
  );
}

function loadPlan(): AutopilotPlan | null {
  if (!dev()) return null;
  try {
    const raw = localStorage.getItem(PLAN_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as AutopilotPlan;
    if (!Array.isArray(parsed.modes) || parsed.modes.length === 0) return null;
    return parsed;
  } catch {
    return null;
  }
}

function savePlan(plan: AutopilotPlan): void {
  try {
    localStorage.setItem(PLAN_KEY, JSON.stringify(plan));
  } catch {
    // Dev-only; never an application correctness dependency.
  }
}

function totalRuns(plan: AutopilotPlan): number {
  return plan.modes.length * plan.repeats;
}

function modeForIndex(plan: AutopilotPlan, index: number): PdfBenchLoadMode {
  return plan.modes[index % plan.modes.length];
}

/** Point the benchmark at the mode this run should use, and make sure it is armed. */
function armMode(mode: PdfBenchLoadMode): void {
  try {
    localStorage.setItem(ENABLED_KEY, "1");
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    // Dev-only.
  }
}

/** The mode the plan wants for the current load, or null when no plan is active. */
export function autopilotCurrentMode(): PdfBenchLoadMode | null {
  const plan = loadPlan();
  if (!plan || plan.done) return null;
  return modeForIndex(plan, plan.index);
}

/**
 * Called from benchmark `finalize()`. Records a content-free diagnostics breadcrumb, advances plan,
 * & either reloads for next mode or reports matrix complete. Full result is native JSONL.
 */
export function autopilotOnComplete(): void {
  const plan = loadPlan();
  if (!plan || plan.done) return;

  const of = totalRuns(plan);
  logEvent("pdf-bench-autopilot-result");

  plan.index += 1;
  if (plan.index >= of) {
    plan.done = true;
    savePlan(plan);
    logEvent("pdf-bench-autopilot-done");
    return;
  }

  savePlan(plan);
  armMode(modeForIndex(plan, plan.index));
  // finalize() has already awaited both settle sampling & durable native write. Reload now: WKWebView
  // can indefinitely throttle a timer once its benchmark window becomes occluded, stranding a
  // multi-mode plan after its first valid sample.
  if (import.meta.env.MODE !== "test") location.reload();
}

/** Begin a matrix. Arms the first mode and reloads so the run starts from a clean document load. */
export function startAutopilot(
  label = "unlabelled",
  modes: PdfBenchLoadMode[] = DEFAULT_MODES,
  repeats = DEFAULT_REPEATS,
  reload = true,
  runId = String(import.meta.env.VITE_PDF_BENCH_RUN_ID ?? label),
): void {
  if (!dev()) return;
  const plan: AutopilotPlan = { runId, modes, repeats, index: 0, label, done: false };
  savePlan(plan);
  armMode(modeForIndex(plan, 0));
  logEvent("pdf-bench-autopilot-start");
  if (reload) location.reload();
}

export function stopAutopilot(): void {
  try {
    localStorage.removeItem(PLAN_KEY);
    localStorage.removeItem(ENABLED_KEY);
  } catch {
    // Dev-only.
  }
}

export function autopilotStatus(): AutopilotPlan | null {
  return loadPlan();
}

/**
 * Auto-start from the environment so the matrix needs no console at all:
 *   VITE_PDF_BENCH_AUTOPILOT=1 pnpm dev:tauri <file.pdf>
 *
 * Only fires when no plan exists yet, so the per-run reloads do not restart the matrix.
 */
export function maybeAutoStartFromEnv(): void {
  if (!dev()) return;
  if (import.meta.env.VITE_PDF_BENCH_AUTOPILOT !== "1") return;
  const label = String(import.meta.env.VITE_PDF_BENCH_LABEL ?? "auto");
  const runId = String(import.meta.env.VITE_PDF_BENCH_RUN_ID ?? label);
  const modes = String(import.meta.env.VITE_PDF_BENCH_MODES ?? "")
    .split(",")
    .map((mode) => mode.trim())
    .filter((mode): mode is PdfBenchLoadMode => DEFAULT_MODES.includes(mode as PdfBenchLoadMode));
  const existing = loadPlan();
  // A plan for THIS label that is still mid-matrix must never be restarted — that's the
  // per-run reload case. But localStorage survives across app launches (same webview profile),
  // so a plan left over from an earlier session/label (done, or for a different fixture) must
  // NOT silently block every future auto-start; start fresh for a new/changed label.
  if (existing && !existing.done && existing.runId === runId) return;
  const repeats = Number(import.meta.env.VITE_PDF_BENCH_REPEATS ?? DEFAULT_REPEATS);
  startAutopilot(
    label,
    modes.length > 0 ? modes : DEFAULT_MODES,
    Number.isFinite(repeats) && repeats > 0 ? repeats : DEFAULT_REPEATS,
    false,
    runId,
  );
}
