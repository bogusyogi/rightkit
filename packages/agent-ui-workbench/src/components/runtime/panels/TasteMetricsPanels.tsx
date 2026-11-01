import type { RoleMetric, TasteScore } from "@rightkit/agent-ui-core/lib/types";
import { Dimension, RuntimeEmpty, ScoreBar, panelWrap, scoreColor } from "./shared";

export function TastePanel({ rows }: { rows: TasteScore[] }) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No taste signal yet"
        body="The evaluation engine scores each agent role on quality, reliability, and safety as runs accumulate. Complete a few goals to populate the leaderboard."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((r, i) => (
        <div
          key={r.role}
          className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3"
        >
          <div className="flex items-center gap-3">
            <span className="w-6 h-6 grid place-items-center rounded-md bg-panel-2 text-[12px] font-bold text-text-3">
              {i + 1}
            </span>
            <strong className="text-[14px] font-semibold text-text flex-1 truncate">
              {r.role}
            </strong>
            <span className="text-[11px] text-text-3">{r.samples} runs</span>
            <span
              className="text-[13px] font-bold font-mono tabular-nums px-2 py-0.5 rounded-md"
              style={{
                color: scoreColor(r.overall),
                background: "color-mix(in srgb, currentColor 12%, transparent)",
              }}
            >
              {r.overall.toFixed(2)}
            </span>
          </div>
          <div className="grid grid-cols-2 sm:grid-cols-3 gap-x-5 gap-y-2.5">
            <Dimension label="GDS" value={r.gds} />
            <Dimension label="Quality" value={r.quality} />
            <Dimension label="Reliability" value={r.reliability} />
            <Dimension label="Consistency" value={r.consistency} />
            <Dimension label="Robustness" value={r.robustness} />
            <Dimension label="Predictability" value={r.predictability} />
            <Dimension label="Safety" value={r.safety} />
            <Dimension label="Ctx efficiency" value={r.contextEfficiency} />
            <Dimension label="Long-horizon" value={r.longHorizonReliability} />
          </div>
          <div className="flex items-center gap-2 pt-1 text-[11px]">
            <span className="text-text-3">VAF</span>
            <span className="font-mono tabular-nums text-text-2">{r.vaf.toFixed(2)}</span>
            <span className="mx-1 text-line">·</span>
            <span className="text-text-3">MOP rate</span>
            <span
              className="font-mono tabular-nums"
              style={{
                color: r.mopRate > 0.1 ? "var(--color-danger)" : "var(--color-accent-2)",
              }}
            >
              {(r.mopRate * 100).toFixed(0)}%
            </span>
            <span className="mx-1 text-line">·</span>
            <span className="text-text-3">decay slope</span>
            <span className="font-mono tabular-nums text-text-2">
              {r.reliabilityDecaySlope.toFixed(3)}
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}

// ── per-role metrics ───────────────────────────────────────────────────────

export function MetricsPanel({ rows }: { rows: RoleMetric[] }) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No role metrics yet"
        body="Success rates per agent role appear after the Conductor routes work to them. Worst performers sort first."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((m) => (
        <div
          key={m.role}
          className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-2"
        >
          <div className="flex items-center gap-3">
            <strong className="text-[13px] font-semibold text-text flex-1 truncate">
              {m.role}
            </strong>
            <span className="text-[11px] text-text-3">
              {m.successes}/{m.runs} ok
            </span>
            <span
              className="text-[13px] font-bold font-mono tabular-nums"
              style={{ color: scoreColor(m.successRate) }}
            >
              {(m.successRate * 100).toFixed(0)}%
            </span>
          </div>
          <ScoreBar value={m.successRate} />
        </div>
      ))}
    </div>
  );
}

// ── benchmark evidence ────────────────────────────────────────────────────
