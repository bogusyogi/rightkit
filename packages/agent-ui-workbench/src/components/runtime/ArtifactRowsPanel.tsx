import Icon from "@rightkit/agent-ui-core/components/Icon";
import { EmptyState } from "@rightkit/react-shell/react";
import type { ArtifactRow } from "@rightkit/agent-ui-core/lib/types";
import ArtifactTrustBadge from "./ArtifactTrustBadge.js";

// D14 — ArtifactRowsPanel.
//
// Hermes Agent's artifact preview makes the artifact card itself the entry
// point to a durable preview. the host app keeps its stronger daemon-owned
// ArtifactRow/trust contract and exposes two explicit actions: Open asks the
// owning surface to choose the right document/file view, while Reveal opens
// the underlying workspace path. Neither action fabricates artifact content.

export interface ArtifactRowsPanelProps {
  artifacts: ArtifactRow[];
  /** Open the artifact in the owning persistent document/file surface. */
  onOpen?: (artifact: ArtifactRow) => void;
  /** Reveal the real workspace-backed artifact path. */
  onReveal?: (path: string) => void;
}

export default function ArtifactRowsPanel({ artifacts, onOpen, onReveal }: ArtifactRowsPanelProps) {
  if (artifacts.length === 0) {
    return (
      <EmptyState icon={<Icon name="attachment" size={18} />} title="No artifacts yet">{"Files the agent writes, patches, and verified outputs appear here as runs produce them."}</EmptyState>
    );
  }

  return (
    <>
      {artifacts.map((artifact) => {
        const title = artifact.path || artifact.id;
        const verdictSummary = artifact.verdict?.summary || artifact.provenance || artifact.meta;
        const hasPath = Boolean(artifact.path.trim());
        const open = hasPath
          ? onOpen
            ? () => onOpen(artifact)
            : onReveal
              ? () => onReveal(artifact.path)
              : null
          : null;

        return (
          <div
            className="grid grid-cols-[minmax(0,1fr)_minmax(180px,0.7fr)_auto_auto_auto] items-center gap-4 px-5 py-3"
            key={artifact.id}
            data-testid={`artifact-row-${artifact.id}`}
          >
            <div className="min-w-0">
              <strong className="block truncate text-[13px] font-semibold text-text">{title}</strong>
              <span className="block truncate text-[11px] text-text-3">
                {artifact.kind} - {artifact.producer}
              </span>
            </div>

            <div className="min-w-0 text-right">
              <span className="block truncate text-[12px] text-text-2">
                {verdictSummary || "No trust verdict yet"}
              </span>
              <code className="block truncate font-mono text-[11px] text-text-3">
                {artifact.contentHash}
              </code>
              <span className="block truncate text-[11px] text-text-3">
                Approval count: {artifact.approvals.length}
              </span>
            </div>

            <ArtifactTrustBadge artifact={artifact} />

            {open ? (
              <button
                type="button"
                aria-label={`Open ${title}`}
                title="Open in the persistent artifact/document surface"
                data-testid={`artifact-row-${artifact.id}-open`}
                onClick={open}
                className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-accent/60 bg-accent px-2 text-[11px] font-semibold text-on-accent hover:border-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
              >
                Open
              </button>
            ) : null}

            {onReveal && hasPath ? (
              <button
                type="button"
                aria-label={`Reveal ${title}`}
                title="Open the real workspace file (live read + fingerprint-guarded save)"
                data-testid={`artifact-row-${artifact.id}-reveal`}
                onClick={() => onReveal(artifact.path)}
                className="cr-control-pill inline-flex h-7 cursor-pointer items-center rounded-md border border-line bg-panel px-2 text-[11px] font-medium text-text hover:border-accent/60 hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
              >
                Reveal
              </button>
            ) : null}
          </div>
        );
      })}
    </>
  );
}
