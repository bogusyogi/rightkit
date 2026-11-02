import type { ArtifactRow } from "@rightkit/agent-ui-core/lib/types";
import { artifactTrustState } from "./runtimeUtils.js";

export default function ArtifactTrustBadge({ artifact }: { artifact: ArtifactRow }) {
  const state = artifactTrustState(artifact);
  const color =
    state.tone === "good"
      ? "border-accent-2 bg-[color-mix(in_srgb,var(--color-accent-2)_10%,transparent)] text-accent-2"
      : state.tone === "warn"
        ? "border-warning bg-[color-mix(in_srgb,var(--color-warning)_10%,transparent)] text-warning"
        : state.tone === "danger"
          ? "border-danger bg-[color-mix(in_srgb,var(--color-danger)_8%,transparent)] text-danger"
          : "border-line bg-panel-2 text-text-3";

  return (
    <span
      className={`inline-flex max-w-full items-center rounded-md border px-2 py-1 text-[11px] font-semibold ${color}`}
      title={state.title}
    >
      <span className="truncate">{state.label}</span>
    </span>
  );
}
