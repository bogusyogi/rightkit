import type { ReadinessSnapshot, ReadinessVerdict, ReadinessState } from "@rightkit/agent-ui-core/lib/daemon";

const VERDICT_COLOR: Record<ReadinessVerdict, string> = {
  pass: "text-accent-2 border-accent-2/40",
  warn: "text-warning border-warning/40",
  block: "text-danger border-danger/40",
};

const STATE_COLOR: Record<ReadinessState, string> = {
  ready: "text-accent-2",
  degraded: "text-warning",
  unavailable: "text-text-3",
  blocked: "text-danger",
};

function titleCase(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

export default function ReadinessCard({ snapshot }: { snapshot: ReadinessSnapshot }) {
  const blockingRows = snapshot.rows.filter((r) => r.verdict === "block");

  return (
    <div className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <span className="text-[12px] font-semibold uppercase tracking-wider text-text-3">
          Setup readiness
        </span>
        <span
          className={`rounded-full border px-2 py-0.5 text-[11px] font-medium ${VERDICT_COLOR[snapshot.verdict]}`}
        >
          {titleCase(snapshot.verdict)}
        </span>
      </div>

      <div className="flex flex-col gap-1.5">
        {snapshot.rows.map((row) => (
          <div key={row.id} className="flex items-start gap-3">
            <span className={`mt-0.5 text-[11px] w-20 shrink-0 ${STATE_COLOR[row.state]}`}>
              {titleCase(row.state)}
            </span>
            <div className="min-w-0 flex-1">
              <span className="text-[12px] text-text">{row.label}</span>
              {row.detail && <span className="ml-1.5 text-[11px] text-text-3">{row.detail}</span>}
              {row.verdict !== "pass" && row.fix && (
                <p className="mt-0.5 text-[11px] text-text-2">{row.fix}</p>
              )}
            </div>
          </div>
        ))}
      </div>

      {blockingRows.length > 0 && (
        <p className="text-[11px] text-text-3 border-t border-line pt-2">
          {blockingRows.length} blocking issue{blockingRows.length !== 1 ? "s" : ""} — resolve
          before running agent sessions.
        </p>
      )}
    </div>
  );
}
