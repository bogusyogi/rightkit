export function scoreColor(v: number): string {
  if (v >= 0.85) return "var(--color-accent-2)"; // green — strong
  if (v >= 0.6) return "var(--color-warning)"; // amber — fair
  return "var(--color-text-3)"; // muted — weak
}

export function ScoreBar({ value, color }: { value: number; color?: string }) {
  const pct = Math.max(0, Math.min(1, value)) * 100;
  return (
    <div className="h-1.5 w-full rounded-full bg-panel-2 overflow-hidden">
      <div
        className="h-full w-full origin-left rounded-full transition-transform"
        style={{ transform: `scaleX(${pct / 100})`, background: color ?? scoreColor(value) }}
      />
    </div>
  );
}

export function Dimension({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <span className="text-[10px] uppercase tracking-wider text-text-3">{label}</span>
        <span className="text-[11px] font-mono tabular-nums text-text-2">{value.toFixed(2)}</span>
      </div>
      <ScoreBar value={value} />
    </div>
  );
}

export function RuntimeEmpty({ title, body }: { title: string; body: string }) {
  return (
    <div className="flex flex-col gap-2 m-auto text-center max-w-[420px] py-12">
      <p className="text-[11px] uppercase text-text-3 font-semibold">Empty</p>
      <h2 className="text-[20px] font-bold text-text">{title}</h2>
      <p className="text-[13px] text-text-2 leading-relaxed">{body}</p>
    </div>
  );
}

export const panelWrap = "flex-1 min-h-0 overflow-y-auto p-5 flex flex-col gap-3";

export function formatPanelTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}
