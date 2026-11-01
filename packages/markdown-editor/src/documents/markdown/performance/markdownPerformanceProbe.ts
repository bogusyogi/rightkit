export type MarkdownPerformanceOperation =
  | "input-latency"
  | "newline-edit"
  | "local-source-patch"
  | "undo-redo"
  | "paste"
  | "source-render-switch";

export interface MarkdownPerformanceBudget {
  readonly operation: MarkdownPerformanceOperation;
  readonly p95Ms: number;
}

export interface MarkdownPerformanceProbeResult {
  readonly operation: MarkdownPerformanceOperation;
  readonly samplesMs: readonly number[];
  readonly p95Ms: number;
}

export interface MarkdownPerformanceProbeOptions {
  readonly warmup?: number;
  readonly samples?: number;
  readonly now?: () => number;
  readonly afterSample?: () => void;
}

/** M5 budgets from docs/plans/2026-08-13-block-local-editing-repair.md, kept local so probes cannot change app policy. */
export const M5_MARKDOWN_PERFORMANCE_BUDGETS: Readonly<
  Record<MarkdownPerformanceOperation, number>
> = {
  "input-latency": 32,
  /** Enter, soft break and paragraph join: same interactive budget as typing. */
  "newline-edit": 32,
  "local-source-patch": 32,
  "undo-redo": 50,
  paste: 100,
  "source-render-switch": 150,
};

export function percentile(values: readonly number[], percentileValue: number): number {
  if (values.length === 0) return 0;
  if (percentileValue <= 0) return Math.min(...values);
  if (percentileValue >= 1) return Math.max(...values);
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.max(0, Math.ceil(sorted.length * percentileValue) - 1);
  return sorted[index];
}

/** Run a fixed-count warm probe with deterministic percentile selection. */
export function runMarkdownPerformanceProbe(
  operation: MarkdownPerformanceOperation,
  callback: () => void,
  options: MarkdownPerformanceProbeOptions = {},
): MarkdownPerformanceProbeResult {
  const warmup = options.warmup ?? 2;
  const samples = options.samples ?? 7;
  if (!Number.isInteger(warmup) || warmup < 0)
    throw new Error("warmup must be a non-negative integer");
  if (!Number.isInteger(samples) || samples < 1)
    throw new Error("samples must be a positive integer");

  const now = options.now ?? (() => performance.now());
  const run = () => {
    const started = now();
    let elapsed = 0;
    try {
      callback();
    } finally {
      elapsed = now() - started;
      options.afterSample?.();
    }
    return elapsed;
  };

  for (let index = 0; index < warmup; index += 1) run();
  const samplesMs = Array.from({ length: samples }, run);
  return {
    operation,
    samplesMs,
    p95Ms: percentile(samplesMs, 0.95),
  };
}

export function assertMarkdownPerformanceBudget(result: MarkdownPerformanceProbeResult): void {
  const budget = M5_MARKDOWN_PERFORMANCE_BUDGETS[result.operation];
  if (result.p95Ms > budget) {
    throw new Error(
      `${result.operation} p95 ${result.p95Ms.toFixed(2)}ms exceeded M5 budget ${budget}ms`,
    );
  }
}

// Re-export performance event counters so tests importing from the
// probe module can also access the event-count counters.
export * from "@rightkit/document-core/documents/perfEventCounters";
