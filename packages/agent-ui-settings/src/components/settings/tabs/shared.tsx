import type { DiagnosticsReport } from "@rightkit/agent-ui-core/lib/types";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export function diagnosticsLabel(status: DiagnosticsReport["verdict"]) {
  if (status === "pass") return "Pass";
  if (status === "warn") return "Warn";
  return "Block";
}

// Single source of truth for status→color: warn is always amber (--color-warning),
// never the command-accent red or the success green. Every status pill/card in
// Settings (Diagnostics checks, Execution backend cards, MCP server-mode cards)
// must read its tone from here so "warn" can't drift back to red in one spot.
export function diagnosticsStatusClass(status: DiagnosticsReport["verdict"]) {
  if (status === "pass") return "border-accent-2/40 text-accent-2";
  if (status === "warn") return "border-warning/40 text-warning";
  return "border-danger/40 text-danger";
}

export function diagnosticsCardStatusClass(status: DiagnosticsReport["verdict"]) {
  if (status === "pass") return "border-accent-2/40 bg-accent-2/10";
  if (status === "warn") return "border-warning/40 bg-warning/10";
  return "border-danger/40 bg-danger/10";
}

export function OfflinePanel({ title, action }: { title: string; action: string }) {
  return (
    <div className="flex items-start gap-3 p-4 rounded-xl border border-line bg-panel text-text-3">
      <Icon name="activity" />
      <div className="flex flex-col gap-0.5">
        <strong className="text-[13px] font-semibold text-text-2">{title}</strong>
        <span className="text-[12px]">{action}</span>
      </div>
    </div>
  );
}

export function NumberField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="flex flex-col gap-1 text-[13px] text-text-2">
      {label}
      <input
        className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
        type="number"
        step="0.01"
        value={Number.isFinite(value) ? value : 0}
        onChange={(e) => onChange(Number(e.target.value))}
      />
    </label>
  );
}

export function DiagnosticsStat({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail?: string;
}) {
  return (
    <div className="rounded-lg border border-line bg-panel-2 px-3 py-2 min-w-0">
      <div className="text-[11px] uppercase text-text-3">{label}</div>
      <div className="text-[13px] text-text truncate" title={value}>
        {value}
      </div>
      {detail ? (
        <div className="text-[11px] text-text-3 truncate" title={detail}>
          {detail}
        </div>
      ) : null}
    </div>
  );
}
