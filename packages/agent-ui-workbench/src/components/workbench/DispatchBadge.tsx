import type { DispatchDecision } from "@rightkit/agent-ui-core/lib/types";

export function DispatchBadge({ decision }: { decision: DispatchDecision | null }) {
  if (!decision) return null;

  // Dispatch strategy is metadata, not a pass/fail status — brand.md reserves
  // the accent color for interactive/current elements, never a status label.
  // Distinguish "multi-agent" from "direct" by weight, not color.
  const label = decision.strategy === "multi-agent" ? "multi-agent" : "direct";
  const tone =
    label === "multi-agent"
      ? "border-line-2 bg-panel-2 text-text"
      : "border-line bg-panel text-text-2";

  return (
    <span
      className={`inline-flex h-6 items-center rounded-md border px-2 text-[11px] font-medium ${tone}`}
      title={decision.rationale}
    >
      {label}
    </span>
  );
}
