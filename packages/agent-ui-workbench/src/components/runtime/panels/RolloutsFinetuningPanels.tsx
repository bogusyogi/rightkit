import type { FinetuningRow, RolloutView } from "@rightkit/agent-ui-core/lib/types";
import { RuntimeEmpty, ScoreBar, panelWrap } from "./shared.js";

const STAGE_STYLE: Record<string, string> = {
  canary: "text-warning border-warning/40 bg-warning/10",
  partial: "text-accent-2 border-accent-2/40 bg-accent-2/10",
  full: "text-accent-2 border-accent-2/50 bg-accent-2/15",
  rolledback: "text-danger border-danger/40 bg-danger/10",
};

export function RolloutsPanel({ rows }: { rows: RolloutView[] }) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No active rollouts"
        body="When a challenger model beats the incumbent on a benchmark, the Conductor stages a canary rollout here — advancing through partial to full, or auto-rolling-back on regression."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((r) => {
        const stage = (r.stage || "").toLowerCase();
        return (
          <div
            key={r.role}
            className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3"
          >
            <div className="flex items-center gap-3">
              <strong className="text-[13px] font-semibold text-text flex-1 truncate">
                {r.role}
              </strong>
              <span
                className={`text-[10px] uppercase tracking-wider font-semibold px-2 py-0.5 rounded-md border ${STAGE_STYLE[stage] ?? "text-text-3 border-line bg-panel-2"}`}
              >
                {r.stage}
              </span>
            </div>
            <div className="flex items-center gap-2 text-[12px] font-mono">
              <span className="text-text-3 truncate">{r.fromModel}</span>
              <span className="text-text-3">→</span>
              <span className="text-text truncate">{r.toModel}</span>
            </div>
            <div className="flex items-center gap-2 text-[11px] text-text-3">
              <span>baseline success</span>
              <span className="font-mono tabular-nums text-text-2">
                {(r.baselineRate * 100).toFixed(0)}%
              </span>
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ── fine-tuning readiness ──────────────────────────────────────────────────

export function FinetuningPanel({ rows }: { rows: FinetuningRow[] }) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No fine-tuning candidates yet"
        body="High-quality runs (GDS ≥ 0.90, no meltdown, no escalation) accumulate per role+model. Once enough qualify, the pair becomes a fine-tuning candidate."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((r) => {
        const frac = r.threshold > 0 ? r.qualifyingRecords / r.threshold : 0;
        return (
          <div
            key={`${r.role}-${r.model}`}
            className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-2"
          >
            <div className="flex items-center gap-3">
              <strong className="text-[13px] font-semibold text-text truncate">{r.role}</strong>
              <span className="text-[11px] font-mono text-text-3 flex-1 truncate">{r.model}</span>
              {r.ready ? (
                <span className="text-[10px] uppercase tracking-wider font-semibold px-2 py-0.5 rounded-md text-accent-2 border border-accent-2/50 bg-accent-2/15">
                  Ready
                </span>
              ) : (
                <span className="text-[11px] font-mono tabular-nums text-text-2">
                  {r.qualifyingRecords}/{r.threshold}
                </span>
              )}
            </div>
            <ScoreBar
              value={Math.min(1, frac)}
              color={r.ready ? "var(--color-accent-2)" : undefined}
            />
          </div>
        );
      })}
    </div>
  );
}

// ── taste-engine proposals ─────────────────────────────────────────────────
