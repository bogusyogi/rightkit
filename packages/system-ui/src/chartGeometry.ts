export type Pt = [number, number] | null;

/** Contiguous runs of known points, so a gap in the data is a gap in the line. */
export function runs(points: Pt[]): [number, number][][] {
  const out: [number, number][][] = [];
  let run: [number, number][] = [];
  for (const p of points) {
    if (p) run.push(p);
    else if (run.length) {
      out.push(run);
      run = [];
    }
  }
  if (run.length) out.push(run);
  return out;
}

const fmt = (n: number) => n.toFixed(1);
export const line = (run: [number, number][]) => run.map(([x, y], i) => `${i ? "L" : "M"}${fmt(x)} ${fmt(y)}`).join("");
export const area = (run: [number, number][], h: number) =>
  `${line(run)}L${fmt(run[run.length - 1][0])} ${h}L${fmt(run[0][0])} ${h}Z`;

export function niceMax(v: number): number {
  if (v <= 0) return 1;
  const pow = 10 ** Math.floor(Math.log10(v));
  const f = v / pow;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * pow;
}

export function known(series: { values: (number | null)[] }[]): number[] {
  return series.flatMap((s) => s.values.filter((v): v is number => v != null));
}

export function chartWindow<T extends { values: (number | null)[] }>(times: number[], series: T[], rangeMs: number) {
  const end = times.length ? times[times.length - 1] : 0;
  const from = Math.max(0, times.findIndex((t) => t >= end - rangeMs));
  const visible = times.slice(from);
  // With less history than the range, the samples we have fill the whole width.
  const short = times.length > 0 && times[0] > end - rangeMs;
  const start = short ? times[0] : end - rangeMs;
  const span = Math.max(end - start, 1);
  const shown = series.map((s) => ({ ...s, values: s.values.slice(from) }));
  return { start, span, short, shown, visible };
}

export function nearestChartPoint(visible: number[], t: number): number | null {
  if (!visible.length) return null;
  let best = 0;
  for (let i = 1; i < visible.length; i++) {
    if (Math.abs(visible[i] - t) < Math.abs(visible[best] - t)) best = i;
  }
  return best;
}
