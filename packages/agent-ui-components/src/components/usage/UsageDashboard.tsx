import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
  type TableSortDirection,
} from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  buildUsageWeekGrid,
  formatCost,
  formatTokenCount,
  formatUsageDate,
  getDailyTooltip,
  getDailyValue,
  getMaxDailyValue,
  getModelColorIndex,
  sortUsageRuns,
  USAGE_DATE_RANGES,
  type DailyAggregate,
  type UsageChartMode,
  type UsageDataSource,
  type UsageOverview,
  type UsageScope,
  type UsageSortColumn,
} from "@rightkit/agent-ui-core/lib/usage/usageProjection";
import { hostAppName } from "../../appName";

export interface UsageDashboardProps {
  /** Injected daemon connector. Analytics storage and endpoints stay upstream-owned. */
  dataSource: UsageDataSource;
  /** the host app session navigation supplied by the shell; the dashboard does not own routing. */
  onOpenRun?: (runId: string) => void;
}

const DAY_LABELS = ["Mon", "", "Wed", "", "Fri", "", ""];
const MODEL_COLORS = [
  "var(--color-accent)",
  "var(--color-accent-2)",
  "var(--color-warning)",
  "var(--color-danger)",
  "#a78bfa",
  "#22d3ee",
  "#fb923c",
  "#84cc16",
];

function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <section className={`rounded-xl border border-line bg-panel p-5 ${className}`}>{children}</section>;
}

function Heatmap({ daily, mode }: { daily: DailyAggregate[]; mode: UsageChartMode }) {
  const grid = useMemo(() => buildUsageWeekGrid(daily, mode), [daily, mode]);
  const tone = ["bg-panel-2", "bg-accent/20", "bg-accent/40", "bg-accent/70", "bg-accent"];
  return (
    <div className="grid gap-1.5" data-testid="usage-heatmap">
      <div className="relative h-3 select-none text-[10px] text-text-3" style={{ marginLeft: 30 }}>
        {grid.monthLabels.map((month) => (
          <span key={`${month.label}-${month.col}`} className="absolute whitespace-nowrap" style={{ left: month.col * 13 }}>
            {month.label}
          </span>
        ))}
      </div>
      <div className="flex">
        <div className="flex w-[30px] shrink-0 flex-col text-[10px] text-text-3">
          {DAY_LABELS.map((label) => <span key={label || "spacer"} className="h-[13px] leading-[13px]">{label}</span>)}
        </div>
        <div
          className="overflow-x-auto overflow-y-hidden"
          style={{ display: "grid", gridTemplateRows: "repeat(7, 11px)", gridTemplateColumns: `repeat(${grid.weeks}, 11px)`, gap: 2 }}
        >
          {Array.from({ length: grid.weeks }, (_, col) =>
            Array.from({ length: 7 }, (_, row) => {
              const cell = grid.cells[row][col];
              return cell ? (
                <div
                  key={cell.date}
                  role="img"
                  className={`h-[11px] w-[11px] rounded-[2px] ${tone[cell.level]}`}
                  style={{ gridRow: row + 1, gridColumn: col + 1 }}
                  title={`${cell.date}: ${formatTokenCount(cell.value)}`}
                  aria-label={`${cell.date}: ${formatTokenCount(cell.value)}`}
                />
              ) : (
                <div
                  // biome-ignore lint/suspicious/noArrayIndexKey: empty calendar coordinates have no domain identity.
                  key={`empty-${col}-${row}`}
                  style={{ gridRow: row + 1, gridColumn: col + 1 }}
                />
              );
            }),
          )}
        </div>
      </div>
      <div className="flex items-center justify-end gap-1 text-[10px] text-text-3">
        <span>Less</span>
        {tone.map((className) => <span key={className} className={`h-[11px] w-[11px] rounded-[2px] ${className}`} />)}
        <span>More</span>
      </div>
    </div>
  );
}

function StackedModelChart({ daily, maxDays = 30 }: { daily: DailyAggregate[]; maxDays?: number }) {
  const sliced = useMemo(() => daily.slice(-maxDays), [daily, maxDays]);
  const models = useMemo(() => {
    const names = new Set<string>();
    for (const day of sliced) for (const model of Object.keys(day.modelBreakdown ?? {})) names.add(model);
    return [...names].sort();
  }, [sliced]);
  const max = useMemo(() => Math.max(1, ...sliced.map((day) =>
    Object.values(day.modelBreakdown ?? {}).reduce((sum, value) => sum + value.inputTokens + value.outputTokens, 0) || day.inputTokens + day.outputTokens,
  )), [sliced]);

  return (
    <div className="grid gap-3" data-testid="usage-model-chart">
      <div className="flex h-40">
        <div className="flex shrink-0 flex-col items-end justify-between py-0.5 pr-2 text-[10px] tabular-nums text-text-3">
          <span>{formatTokenCount(max)}</span><span>{formatTokenCount(max / 2)}</span><span>0</span>
        </div>
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="relative flex flex-1 gap-[2px] border-b border-l border-line">
            <div className="pointer-events-none absolute inset-x-0 top-1/2 border-t border-line/60" />
            {sliced.map((day) => {
              const entries = Object.entries(day.modelBreakdown ?? {});
              const total = entries.reduce((sum, [, value]) => sum + value.inputTokens + value.outputTokens, 0) || day.inputTokens + day.outputTokens;
              return (
                <div key={day.date} className="group flex min-w-0 flex-1 cursor-default items-end" title={`${day.date}\n${formatTokenCount(total)} tokens`}>
                  <div className="flex w-full flex-col-reverse overflow-hidden rounded-t" style={{ height: `${Math.max((total / max) * 100, 2)}%` }}>
                    {entries.length > 0 ? entries.map(([model, value]) => (
                      <div key={model} className="min-h-0 w-full opacity-70 group-hover:opacity-100" style={{ height: `${total ? ((value.inputTokens + value.outputTokens) / total) * 100 : 0}%`, backgroundColor: MODEL_COLORS[getModelColorIndex(model)] }} />
                    )) : <div className="h-full w-full bg-accent/60 group-hover:bg-accent" />}
                  </div>
                </div>
              );
            })}
          </div>
          <div className="mt-1 flex gap-[2px]">
            {sliced.map((day, index) => <span key={day.date} className="min-w-0 flex-1 text-center text-[10px] tabular-nums text-text-3">{sliced.length <= 10 || index % Math.ceil(sliced.length / 10) === 0 ? day.date.slice(5) : ""}</span>)}
          </div>
        </div>
      </div>
      {models.length > 0 ? <div className="flex flex-wrap gap-x-3 gap-y-1 text-[10px] text-text-3">
        {models.map((model) => <span key={model} className="flex max-w-[180px] items-center gap-1 truncate" title={model}><i className="h-2.5 w-2.5 shrink-0 rounded-sm" style={{ backgroundColor: MODEL_COLORS[getModelColorIndex(model)] }} />{model}</span>)}
      </div> : null}
    </div>
  );
}

function sortDirection(sortColumn: UsageSortColumn, column: UsageSortColumn, sortAscending: boolean): TableSortDirection {
  return sortColumn === column ? (sortAscending ? "ascending" : "descending") : "none";
}

export function UsageDashboard({ dataSource, onOpenRun }: UsageDashboardProps) {
  const [scope, setScope] = useState<UsageScope>("global");
  const [selectedDays, setSelectedDays] = useState<number | undefined>();
  const [chartMode, setChartMode] = useState<UsageChartMode>("messages");
  const [data, setData] = useState<UsageOverview | null>(null);
  const [heatmapDaily, setHeatmapDaily] = useState<DailyAggregate[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [showFullScanMessage, setShowFullScanMessage] = useState(false);
  const [sortColumn, setSortColumn] = useState<UsageSortColumn>("date");
  const [sortAscending, setSortAscending] = useState(false);
  const requestId = useRef(0);
  const heatmapRequestId = useRef(0);
  const hasLoaded = useRef(false);

  const loadData = useCallback(async (nextScope: UsageScope, days?: number) => {
    const currentRequest = ++requestId.current;
    if (!hasLoaded.current) setLoading(true);
    setError("");
    setShowFullScanMessage(false);
    const timer = window.setTimeout(() => {
      if (currentRequest === requestId.current) setShowFullScanMessage(true);
    }, 500);
    try {
      const result = await dataSource.getOverview(nextScope, days);
      if (currentRequest !== requestId.current) return;
      setData(result);
      hasLoaded.current = true;
    } catch (reason) {
      if (currentRequest === requestId.current) setError(String(reason));
    } finally {
      window.clearTimeout(timer);
      if (currentRequest === requestId.current) {
        setLoading(false);
        setShowFullScanMessage(false);
      }
    }
  }, [dataSource]);

  const loadHeatmapData = useCallback(async (nextScope: UsageScope) => {
    const currentRequest = ++heatmapRequestId.current;
    try {
      const result = await dataSource.getHeatmapDaily(nextScope);
      if (currentRequest === heatmapRequestId.current) setHeatmapDaily(result);
    } catch {
      if (currentRequest === heatmapRequestId.current) setHeatmapDaily(null);
    }
  }, [dataSource]);

  useEffect(() => {
    void loadData(scope, selectedDays);
    void loadHeatmapData(scope);
  }, [loadData, loadHeatmapData, scope, selectedDays]);

  const sortedRuns = useMemo(
    () => sortUsageRuns(data?.runs ?? [], sortColumn, sortAscending),
    [data?.runs, sortAscending, sortColumn],
  );
  const maxDailyValue = useMemo(
    () => getMaxDailyValue(data?.daily ?? [], chartMode),
    [chartMode, data?.daily],
  );

  const selectScope = (nextScope: UsageScope) => {
    setScope(nextScope);
    if (nextScope === "app" && (chartMode === "messages" || chartMode === "sessions")) setChartMode("cost");
  };
  const toggleSort = (column: UsageSortColumn) => {
    if (sortColumn === column) setSortAscending((value) => !value);
    else { setSortColumn(column); setSortAscending(false); }
  };
  const refresh = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await dataSource.clearCache?.();
      await Promise.all([loadData(scope, selectedDays), loadHeatmapData(scope)]);
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <main className="mx-auto max-w-5xl space-y-5 p-6" data-testid="usage-dashboard">
      <header className="flex items-center gap-3">
        <div className="grid h-12 w-12 place-items-center rounded-xl bg-accent/10 text-accent"><Icon name="activity" size={25} /></div>
        <div><h1 className="text-2xl font-semibold text-text">Usage</h1><p className="text-sm text-text-3">Token, cost, and session activity</p></div>
      </header>
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex gap-1 rounded-lg bg-panel-2 p-0.5" role="tablist" aria-label="Usage scope">
          {(["global", "app"] as const).map((value) => <button key={value} type="button" role="tab" aria-selected={scope === value} className={`rounded-md px-3 py-1.5 text-xs font-medium ${scope === value ? "bg-panel text-text shadow-sm" : "text-text-3 hover:text-text"}`} onClick={() => selectScope(value)}>{value === "global" ? "All sessions" : "This app"}</button>)}
        </div>
        <nav className="flex gap-1" aria-label="Usage date range">
          {USAGE_DATE_RANGES.map((range) => <button key={range.label} type="button" className={`rounded-md px-3 py-1.5 text-xs font-medium ${selectedDays === range.days ? "bg-accent text-on-accent" : "bg-panel-2 text-text-3 hover:text-text"}`} onClick={() => setSelectedDays(range.days)}>{range.label}</button>)}
        </nav>
        <button type="button" className="ml-auto grid h-7 w-7 place-items-center rounded-md text-text-3 hover:bg-panel-2 hover:text-text disabled:opacity-40" aria-label="Refresh usage" title="Refresh usage" disabled={refreshing || scope !== "global"} onClick={() => void refresh()}><Icon name="refresh" size={14} /></button>
      </div>
      {scope === "global" ? <p className="text-xs text-text-3">Global usage includes all {hostAppName()} sessions available to the daemon.</p> : null}

      {loading && !data ? <div className="grid justify-items-center gap-3 py-16 text-sm text-text-3"><span className="h-6 w-6 animate-spin rounded-full border-2 border-accent/30 border-t-accent" />{showFullScanMessage ? <span>Scanning usage history…</span> : null}</div> : error ? <div role="alert" className="rounded-lg border border-danger/50 bg-danger/10 px-4 py-3 text-sm text-danger">{error}</div> : data ? <>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Card className="p-4 text-center"><p className="text-2xl font-semibold">{data.costComplete ? formatCost(data.totalCostUsd) : "—"}</p><p className="mt-1 text-xs text-text-3">Total cost</p></Card>
          <Card className="p-4 text-center"><p className="text-2xl font-semibold">{formatTokenCount(data.totalTokens)}</p><p className="mt-1 text-xs text-text-3">Total tokens</p></Card>
          <Card className="p-4 text-center"><p className="text-2xl font-semibold">{data.totalRuns}</p><p className="mt-1 text-xs text-text-3">{scope === "global" ? "Sessions" : "Runs"}</p></Card>
          <Card className="p-4 text-center"><p className="text-2xl font-semibold">{data.currentStreak > 0 ? `${data.currentStreak} days` : data.activeDays}</p><p className="mt-1 text-xs text-text-3">{data.currentStreak > 0 ? `Current streak · ${data.longestStreak} longest` : "Active days"}</p></Card>
        </div>
        {heatmapDaily ? <Card><div className="mb-3 flex items-center justify-between"><h2 className="text-xs font-semibold uppercase tracking-wider text-text-3">Activity</h2><span className="text-xs text-text-3">{data.activeDays} active days · {data.longestStreak} longest streak</span></div><Heatmap daily={heatmapDaily} mode={chartMode} /></Card> : null}
        <Card>
          <fieldset className="mb-4 flex flex-wrap items-center justify-between gap-2"><legend className="sr-only">Daily trend controls</legend><h2 className="text-xs font-semibold uppercase tracking-wider text-text-3">Daily trend</h2>
            <div className="flex gap-1">
{(["cost", "tokens", ...(scope === "global" ? ["messages", "sessions"] : [])] as UsageChartMode[]).map((mode) => <button key={mode} type="button" aria-pressed={chartMode === mode} className={`rounded px-2 py-1 text-[10px] font-medium ${chartMode === mode ? "bg-accent/20 text-accent" : "text-text-3 hover:text-text"}`} onClick={() => setChartMode(mode)}>{mode[0].toUpperCase() + mode.slice(1)}</button>)}</div>
          </fieldset>
          {data.daily.length > 0 ? (scope === "global" && chartMode === "tokens" && data.daily.some((day) => day.modelBreakdown) ? <StackedModelChart daily={data.daily} /> : <div className="flex h-40"><div className="flex shrink-0 flex-col items-end justify-between py-0.5 pr-2 text-[10px] tabular-nums text-text-3"><span>{chartMode === "cost" ? formatCost(maxDailyValue) : formatTokenCount(maxDailyValue)}</span><span>{chartMode === "cost" ? formatCost(maxDailyValue / 2) : formatTokenCount(maxDailyValue / 2)}</span><span>0</span></div><div className="flex min-w-0 flex-1 flex-col"><div className="relative flex flex-1 gap-[2px] border-b border-l border-line">{data.daily.slice(-30).map((day) => <div key={day.date} className="group flex min-w-0 flex-1 cursor-default items-end" title={getDailyTooltip(day, chartMode)}><div className="w-full rounded-t bg-accent/60 group-hover:bg-accent" style={{ height: `${Math.max((getDailyValue(day, chartMode) / maxDailyValue) * 100, 2)}%` }} /></div>)}</div><div className="mt-1 flex gap-[2px]">{data.daily.slice(-30).map((day, index, days) => <span key={day.date} className="min-w-0 flex-1 text-center text-[10px] tabular-nums text-text-3">{days.length <= 10 || index % Math.ceil(days.length / 10) === 0 ? day.date.slice(5) : ""}</span>)}</div></div></div>) : <p className="text-sm text-text-3">No daily usage data.</p>}</Card>
        <Card><h2 className="mb-4 text-xs font-semibold uppercase tracking-wider text-text-3">By model</h2>{data.byModel.length > 0 ? <div className="overflow-x-auto"><Table dense label="Usage by model"><TableHead><TableRow><TableHeaderCell>Model</TableHeaderCell>{scope === "app" ? <TableHeaderCell align="end">Runs</TableHeaderCell> : null}<TableHeaderCell align="end">Input</TableHeaderCell><TableHeaderCell align="end">Output</TableHeaderCell><TableHeaderCell align="end">Cache read</TableHeaderCell><TableHeaderCell align="end">Cache write</TableHeaderCell><TableHeaderCell align="end">Cost</TableHeaderCell><TableHeaderCell align="end">%</TableHeaderCell></TableRow></TableHead><TableBody>{data.byModel.map((model) => <TableRow key={model.model}><TableCell className="max-w-[180px] truncate font-mono text-xs" title={model.model}>{model.model}</TableCell>{scope === "app" ? <TableCell numeric>{model.runs}</TableCell> : null}<TableCell numeric className="font-mono text-xs">{formatTokenCount(model.inputTokens)}</TableCell><TableCell numeric className="font-mono text-xs">{formatTokenCount(model.outputTokens)}</TableCell><TableCell numeric className="font-mono text-xs text-text-3">{formatTokenCount(model.cacheReadTokens)}</TableCell><TableCell numeric className="font-mono text-xs text-text-3">{formatTokenCount(model.cacheWriteTokens)}</TableCell><TableCell numeric className="font-mono text-xs">{model.costComplete ? formatCost(model.costUsd) : "—"}</TableCell><TableCell align="end"><span className="inline-flex items-center gap-1.5"><span className="h-1.5 w-12 overflow-hidden rounded-full bg-panel-2"><span className="block h-full rounded-full bg-accent" style={{ width: `${Math.min(model.pct, 100)}%` }} /></span><span className="w-8 font-mono text-xs tabular-nums text-text-3">{model.costComplete ? `${model.pct.toFixed(0)}%` : "—"}</span></span></TableCell></TableRow>)}</TableBody></Table></div> : <p className="text-sm text-text-3">No model usage data.</p>}</Card>
         {scope === "app" ? <Card>
            <h2 className="mb-4 text-xs font-semibold uppercase tracking-wider text-text-3">Run history</h2>
            {sortedRuns.length > 0 ?
              <div className="overflow-x-auto">
                <Table dense label="Usage history"><TableHead><TableRow><TableHeaderCell sort={sortDirection(sortColumn, "date", sortAscending)} onSort={() => toggleSort("date")}>Date</TableHeaderCell><TableHeaderCell>Name</TableHeaderCell><TableHeaderCell>Model</TableHeaderCell><TableHeaderCell align="end" sort={sortDirection(sortColumn, "tokens", sortAscending)} onSort={() => toggleSort("tokens")}>Tokens</TableHeaderCell><TableHeaderCell align="end" sort={sortDirection(sortColumn, "cost", sortAscending)} onSort={() => toggleSort("cost")}>Cost</TableHeaderCell><TableHeaderCell align="end" sort={sortDirection(sortColumn, "turns", sortAscending)} onSort={() => toggleSort("turns")}>Turns</TableHeaderCell></TableRow></TableHead><TableBody>{sortedRuns.map((run) => <TableRow key={run.runId}><TableCell className="whitespace-nowrap text-xs text-text-3">{formatUsageDate(run.startedAt)}</TableCell><TableCell className="max-w-[200px] truncate" title={run.name}>{onOpenRun ? <button type="button" className="max-w-full truncate text-left hover:text-accent" onClick={() => onOpenRun(run.runId)}>{run.name}</button> : run.name}</TableCell><TableCell className="max-w-[140px] truncate font-mono text-xs text-text-3" title={run.model ?? run.agent}>{run.model ?? "—"}</TableCell><TableCell numeric className="font-mono text-xs">{formatTokenCount(run.inputTokens + run.outputTokens)}</TableCell><TableCell numeric className="font-mono text-xs">{run.costAvailable ? formatCost(run.totalCostUsd) : "—"}</TableCell><TableCell numeric>{run.numTurns}</TableCell></TableRow>)}</TableBody></Table></div> : <p className="text-sm text-text-3">No usage history.</p>}</Card> : null}
      </> : null}
    </main>
  );
}

export default UsageDashboard;
