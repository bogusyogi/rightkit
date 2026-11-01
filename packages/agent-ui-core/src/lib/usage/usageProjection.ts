/**
 * Renderer-side usage projections.
 *
 * This is the React seam for the OpenCovibe usage page. It deliberately owns
 * formatting, sorting, chart values, and heatmap geometry only; usage records
 * remain daemon-owned and are supplied by an injected data source.
 */

export type UsageScope = "app" | "global";
export type UsageChartMode = "cost" | "tokens" | "messages" | "sessions";
export type UsageSortColumn = "date" | "cost" | "tokens" | "turns";

export interface ModelTokens {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
}

export interface ModelAggregate {
  model: string;
  runs: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  costUsd: number;
  costComplete: boolean;
  pct: number;
}

export interface DailyAggregate {
  date: string;
  costUsd: number;
  costComplete: boolean;
  runs: number;
  inputTokens: number;
  outputTokens: number;
  messageCount?: number;
  sessionCount?: number;
  modelBreakdown?: Record<string, ModelTokens>;
}

export interface UsageRunSummary {
  runId: string;
  name: string;
  agent: string;
  model?: string;
  status: string;
  startedAt: string;
  endedAt?: string;
  totalCostUsd: number;
  costAvailable: boolean;
  inputTokens: number;
  outputTokens: number;
  durationMs: number;
  numTurns: number;
  costEstimated?: boolean;
}

export interface UsageOverview {
  totalCostUsd: number;
  costComplete: boolean;
  totalTokens: number;
  totalRuns: number;
  avgCostPerRun: number;
  byModel: ModelAggregate[];
  daily: DailyAggregate[];
  runs: UsageRunSummary[];
  scanMode?: string;
  activeDays: number;
  currentStreak: number;
  longestStreak: number;
}

/** The daemon connector is intentionally injected: this lane does not own an analytics endpoint. */
export interface UsageDataSource {
  getOverview: (scope: UsageScope, days?: number) => Promise<UsageOverview>;
  getHeatmapDaily: (scope: UsageScope) => Promise<DailyAggregate[]>;
  clearCache?: () => Promise<void>;
}

export const USAGE_DATE_RANGES: ReadonlyArray<{ label: string; days?: number }> = [
  { label: "1d", days: 1 },
  { label: "7d", days: 7 },
  { label: "30d", days: 30 },
  { label: "90d", days: 90 },
  { label: "All" },
];

export function formatTokenCount(value: number): string {
  const tokens = Math.max(0, Number.isFinite(value) ? value : 0);
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}m`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(Math.round(tokens));
}

export function formatCost(value: number): string {
  const cost = Math.max(0, Number.isFinite(value) ? value : 0);
  if (cost >= 1) return `$${cost.toFixed(2)}`;
  if (cost >= 0.01) return `$${cost.toFixed(4)}`;
  return `$${cost.toFixed(6)}`;
}

export function getDailyValue(day: DailyAggregate, mode: UsageChartMode): number {
  if (mode === "cost") return day.costComplete ? day.costUsd : 0;
  if (mode === "messages") return day.messageCount ?? 0;
  if (mode === "sessions") return day.sessionCount ?? 0;
  return day.inputTokens + day.outputTokens;
}

export function getDailyTooltip(day: DailyAggregate, mode: UsageChartMode): string {
  const value = getDailyValue(day, mode);
  if (mode === "cost") return `${day.date}\n${day.costComplete ? formatCost(value) : "—"}`;
  if (mode === "messages") return `${day.date}\n${value} messages`;
  if (mode === "sessions") return `${day.date}\n${value} sessions`;
  return `${day.date}\n${formatTokenCount(value)} tokens`;
}

export function getMaxDailyValue(
  daily: readonly DailyAggregate[],
  mode: UsageChartMode,
): number {
  if (daily.length === 0) return 1;
  const floor = mode === "cost" ? 0.01 : 1;
  return Math.max(floor, ...daily.map((day) => getDailyValue(day, mode)));
}

export function sortUsageRuns(
  runs: readonly UsageRunSummary[],
  column: UsageSortColumn,
  ascending: boolean,
): UsageRunSummary[] {
  const sorted = [...runs].sort((a, b) => {
    let comparison: number;
    if (column === "date") comparison = a.startedAt.localeCompare(b.startedAt);
    else if (column === "cost") comparison = a.totalCostUsd - b.totalCostUsd;
    else if (column === "tokens") {
      comparison = a.inputTokens + a.outputTokens - (b.inputTokens + b.outputTokens);
    } else comparison = a.numTurns - b.numTurns;
    return ascending ? comparison : -comparison;
  });
  return sorted;
}

export interface UsageGridCell {
  date: string;
  value: number;
  level: 0 | 1 | 2 | 3 | 4;
}

export interface UsageMonthLabel {
  label: string;
  col: number;
}

export interface UsageWeekGrid {
  cells: (UsageGridCell | null)[][];
  monthLabels: UsageMonthLabel[];
  weeks: number;
}

function percentileThresholds(values: readonly number[]): [number, number, number] {
  const nonZero = values.filter((value) => value > 0).sort((a, b) => a - b);
  if (nonZero.length === 0) return [0, 0, 0];
  const percentile = (percent: number) => nonZero[Math.floor((percent / 100) * (nonZero.length - 1))];
  return [percentile(25), percentile(50), percentile(75)];
}

function valueToLevel(value: number, thresholds: [number, number, number]): 0 | 1 | 2 | 3 | 4 {
  if (value <= 0) return 0;
  if (value <= thresholds[0]) return 1;
  if (value <= thresholds[1]) return 2;
  if (value <= thresholds[2]) return 3;
  return 4;
}

/** OpenCovibe's UTC, right-aligned 53-week calendar projection, ported to the renderer. */
export function buildUsageWeekGrid(
  daily: readonly DailyAggregate[],
  mode: UsageChartMode,
  weeks = 53,
  today = new Date(),
): UsageWeekGrid {
  const minMonthLabelGap = 3;
  const utcToday = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate()));
  const todayDay = (utcToday.getUTCDay() + 6) % 7;
  const startDate = new Date(utcToday);
  startDate.setUTCDate(startDate.getUTCDate() - todayDay - (weeks - 1) * 7);

  const values = new Map(daily.map((day) => [day.date, getDailyValue(day, mode)]));
  const thresholds = percentileThresholds([...values.values()]);
  const cells: (UsageGridCell | null)[][] = Array.from({ length: 7 }, () =>
    Array.from({ length: weeks }, () => null),
  );
  const monthLabels: UsageMonthLabel[] = [];
  let lastMonth = -1;

  for (let col = 0; col < weeks; col += 1) {
    for (let row = 0; row < 7; row += 1) {
      const date = new Date(startDate);
      date.setUTCDate(startDate.getUTCDate() + col * 7 + row);
      if (date > utcToday) continue;
      const dateString = date.toISOString().slice(0, 10);
      cells[row][col] = {
        date: dateString,
        value: values.get(dateString) ?? 0,
        level: valueToLevel(values.get(dateString) ?? 0, thresholds),
      };
      if (row === 0 && date.getUTCMonth() !== lastMonth) {
        const label = date.toLocaleDateString("en-US", { month: "short", timeZone: "UTC" });
        const previous = monthLabels[monthLabels.length - 1];
        if (previous && col - previous.col < minMonthLabelGap) {
          monthLabels[monthLabels.length - 1] = { label, col };
        } else {
          monthLabels.push({ label, col });
        }
        lastMonth = date.getUTCMonth();
      }
    }
  }
  return { cells, monthLabels, weeks };
}

export function getModelColorIndex(modelName: string): number {
  let hash = 0;
  for (let index = 0; index < modelName.length; index += 1) {
    hash = (hash * 31 + modelName.charCodeAt(index)) | 0;
  }
  return ((hash % 8) + 8) % 8;
}

export function formatUsageDate(isoString: string): string {
  const date = new Date(isoString);
  return Number.isNaN(date.getTime())
    ? isoString
    : new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(date);
}
