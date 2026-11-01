/**
 * Performance budget tracking.
 *
 * Tracks the wall-clock cost of analysis, projection, and commit
 * paths. Used by the diagnostics pane to surface regressions and
 * by the test suite as an enforced ceiling.
 */

export interface PerformanceSample {
  readonly name: string;
  readonly durationMs: number;
  readonly timestampMs: number;
}

export interface PerformanceBudgets {
  readonly analyzeMarkdown: number;
  readonly projectToPm: number;
  readonly applyCommand: number;
  /** Tab activation to first painted Markdown content (md:tab-switch:* performance measures). */
  readonly tabSwitchWarm: number;
  readonly tabSwitchCold: number;
}

export const DEFAULT_PERFORMANCE_BUDGETS: PerformanceBudgets = {
  analyzeMarkdown: 50,
  projectToPm: 100,
  applyCommand: 80,
  tabSwitchWarm: 150,
  tabSwitchCold: 400,
};

/** Journey/QA check: names the md:tab-switch:* measures that exceed budget. */
export function tabSwitchViolations(
  entries: ReadonlyArray<{ name: string; duration: number }>,
  budgets: PerformanceBudgets = DEFAULT_PERFORMANCE_BUDGETS,
): string[] {
  return entries
    .filter(
      (e) =>
        (e.name === "md:tab-switch:warm" && e.duration > budgets.tabSwitchWarm) ||
        (e.name === "md:tab-switch:cold" && e.duration > budgets.tabSwitchCold),
    )
    .map((e) => `${e.name} ${e.duration.toFixed(1)}ms`);
}

const SAMPLES_LIMIT = 256;

export class PerformanceBudgetTracker {
  private readonly samples: PerformanceSample[] = [];
  private readonly budgets: PerformanceBudgets;

  constructor(budgets: PerformanceBudgets = DEFAULT_PERFORMANCE_BUDGETS) {
    this.budgets = budgets;
  }

  record(name: keyof PerformanceBudgets, durationMs: number): void {
    this.samples.push({
      name,
      durationMs,
      timestampMs: performance.now(),
    });
    if (this.samples.length > SAMPLES_LIMIT) this.samples.shift();
  }

  /**
   * Returns samples exceeding the configured budget. Callers
   * usually log or display the violations.
   */
  violations(): readonly PerformanceSample[] {
    return this.samples.filter((s) => {
      const budget = this.budgets[s.name as keyof PerformanceBudgets];
      return typeof budget === "number" && s.durationMs > budget;
    });
  }

  /**
   * Average duration across the last N samples for `name`.
   */
  average(name: keyof PerformanceBudgets, lastN = 10): number {
    const filtered = this.samples.filter((s) => s.name === name).slice(-lastN);
    if (filtered.length === 0) return 0;
    let total = 0;
    for (const sample of filtered) total += sample.durationMs;
    return total / filtered.length;
  }

  clear(): void {
    this.samples.length = 0;
  }
}

/**
 * Measure the wall-clock cost of `fn` and record it under `name`.
 * Returns whatever `fn` returned.
 */
export function measurePerformance<T>(
  tracker: PerformanceBudgetTracker,
  name: keyof PerformanceBudgets,
  fn: () => T,
): T {
  const start = performance.now();
  try {
    return fn();
  } finally {
    tracker.record(name, performance.now() - start);
  }
}
