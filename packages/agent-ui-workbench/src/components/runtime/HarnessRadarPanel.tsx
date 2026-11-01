import { EmptyState } from "@rightkit/react-shell/react";
import type { HarnessRadarSnapshot } from "@rightkit/agent-ui-core/lib/types";
import RuntimeStat from "./RuntimeStat";
import { phaseLabel } from "./runtimeUtils";
export default function HarnessRadarPanel({
  snapshot,
  pendingPromotionId,
  schedulePending,
  onPromoteProposal,
  onScheduleRefresh,
}: {
  snapshot: HarnessRadarSnapshot;
  pendingPromotionId: string | null;
  schedulePending: boolean;
  onPromoteProposal: (id: string) => void;
  onScheduleRefresh: () => void;
}) {
  const capabilityEntries = snapshot.capabilities.map((capability) => ({
    capability,
    count: snapshot.capabilityMap[capability] ?? 0,
  }));
  const primaryValidation = snapshot.validations[0];
  const actionableChanges = snapshot.changes.filter((change) => change.status !== "unchanged");
  const changesByProject = new Map(snapshot.changes.map((change) => [change.projectName, change]));

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="grid grid-cols-4 gap-3 border-b border-line px-5 py-3">
        <RuntimeStat label="Sources" value={snapshot.sources.length} />
        <RuntimeStat label="Projects" value={snapshot.entries.length} />
        <RuntimeStat label="Changes" value={actionableChanges.length} />
        <RuntimeStat label="Proposals" value={snapshot.proposals.length} />
      </div>
      {primaryValidation && (
        <div className="border-b border-line px-5 py-3">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-[11px] font-semibold uppercase text-text-3">
              Source validation
            </span>
            <span
              className={`rounded border px-2 py-0.5 text-[10px] font-semibold uppercase ${
                primaryValidation.status === "pass"
                  ? "border-accent-2/40 text-accent-2"
                  : primaryValidation.status === "warn"
                    ? "border-warning/40 text-warning"
                    : "border-danger/40 text-danger"
              }`}
            >
              {primaryValidation.status}
            </span>
            <code className="rounded border border-line bg-panel px-2 py-0.5 text-[11px] text-accent-2">
              {primaryValidation.sourceId}
            </code>
            <span className="text-[11px] text-text-3">
              {primaryValidation.entryCount} entries · {primaryValidation.capabilityCount}{" "}
              capabilities · snapshot {primaryValidation.snapshotUpdatedAt || "undated"}
            </span>
            <button
              type="button"
              className="ml-auto rounded border border-line bg-panel px-2 py-1 text-[11px] text-text-2 disabled:cursor-not-allowed disabled:opacity-50"
              disabled={schedulePending}
              onClick={onScheduleRefresh}
            >
              {schedulePending ? "Scheduling" : "Schedule weekly refresh"}
            </button>
          </div>
          {primaryValidation.issues.length > 0 && (
            <div className="mt-2 flex flex-col gap-1">
              {primaryValidation.issues.slice(0, 3).map((issue) => (
                <p
                  className="text-[11px] leading-relaxed text-text-3"
                  key={`${issue.code}:${issue.message}`}
                >
                  {issue.severity.toUpperCase()} · {issue.message}
                </p>
              ))}
            </div>
          )}
        </div>
      )}
      <div className="grid grid-cols-[minmax(0,0.9fr)_minmax(0,1.2fr)_minmax(220px,0.8fr)] divide-x divide-line">
        <div className="min-w-0">
          <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
            Capability map
          </div>
          <div className="flex flex-col divide-y divide-line">
            {capabilityEntries.length === 0 ? (
              <EmptyState title="No capabilities mapped">{"As the harness watches projects, the capabilities it exercises are tallied here."}</EmptyState>
            ) : null}
            {capabilityEntries.map(({ capability, count }) => (
              <div className="flex items-center gap-3 px-5 py-3" key={capability}>
                <div className="min-w-0 flex-1">
                  <strong className="block truncate text-[13px] font-semibold text-text">
                    {phaseLabel(capability)}
                  </strong>
                  <span className="block truncate text-[11px] text-text-3">
                    {count} watched project{count === 1 ? "" : "s"}
                  </span>
                </div>
                <code className="rounded border border-line bg-panel px-2 py-1 text-[11px] text-accent-2">
                  {count}
                </code>
              </div>
            ))}
          </div>
        </div>
        <div className="min-w-0">
          <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
            Watched projects
          </div>
          {snapshot.entries.length === 0 ? (
            <EmptyState title="No radar entries">{"Harness Radar entries appear after an offline snapshot is imported by the daemon."}</EmptyState>
          ) : (
            <div className="flex flex-col divide-y divide-line">
              {snapshot.entries.map((entry) => (
                <div className="px-5 py-3" key={`${entry.sourceId}:${entry.projectName}`}>
                  <div className="flex items-center gap-2">
                    <strong className="min-w-0 flex-1 truncate text-[13px] font-semibold text-text">
                      {entry.projectName}
                    </strong>
                    {changesByProject.get(entry.projectName) && (
                      <span
                        className={`rounded border px-2 py-0.5 text-[10px] font-semibold uppercase ${
                          changesByProject.get(entry.projectName)?.status === "new"
                            ? "border-accent-2/40 text-accent-2"
                            : changesByProject.get(entry.projectName)?.status === "changed"
                              ? "border-warning/40 text-warning"
                              : "border-line text-text-3"
                        }`}
                      >
                        {changesByProject.get(entry.projectName)?.status}
                      </span>
                    )}
                    <span className="rounded border border-line bg-panel px-2 py-0.5 text-[10px] text-text-3">
                      {entry.category}
                    </span>
                  </div>
                  <p className="mt-1 line-clamp-2 text-[12px] leading-relaxed text-text-2">
                    {entry.summary || "No summary in radar snapshot."}
                  </p>
                  <div className="mt-2 flex flex-wrap gap-1">
                    {entry.tags.map((tag) => (
                      <span
                        className="rounded border border-line bg-panel-2 px-1.5 py-0.5 text-[10px] text-text-3"
                        key={tag}
                      >
                        {tag}
                      </span>
                    ))}
                  </div>
                  <p className="mt-2 truncate text-[11px] text-text-3">
                    {entry.updatedAt || "No update date"} · {entry.repoUrl}
                  </p>
                  {changesByProject.get(entry.projectName)?.changedFields.length ? (
                    <p className="mt-1 truncate text-[11px] text-text-3">
                      Changed: {changesByProject.get(entry.projectName)?.changedFields.join(", ")}
                    </p>
                  ) : null}
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="min-w-0">
          <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
            Proposal queue
          </div>
          <div className="flex flex-col divide-y divide-line">
            {snapshot.proposals.length === 0 && snapshot.notes.length === 0 ? (
              <EmptyState title="No proposals">{"Promotion proposals appear here once the evolution layer has enough signal to suggest a model or prompt change."}</EmptyState>
            ) : null}
            {snapshot.proposals.map((proposal) => (
              <div className="px-5 py-3" key={proposal.id}>
                <div className="flex items-start gap-2">
                  <strong className="min-w-0 flex-1 text-[13px] font-semibold text-text">
                    {proposal.title}
                  </strong>
                  <button
                    type="button"
                    className="rounded border border-line bg-panel px-2 py-1 text-[11px] text-text-2 disabled:cursor-not-allowed disabled:opacity-50"
                    disabled={pendingPromotionId === proposal.id}
                    onClick={() => onPromoteProposal(proposal.id)}
                  >
                    {pendingPromotionId === proposal.id ? "Queuing" : "Promote"}
                  </button>
                </div>
                <p className="mt-1 text-[12px] leading-relaxed text-text-2">{proposal.rationale}</p>
                <p className="mt-2 truncate text-[11px] text-text-3">
                  {phaseLabel(proposal.capability)} · {proposal.projectNames.join(", ")}
                </p>
              </div>
            ))}
            {snapshot.notes.map((note) => (
              <div className="px-5 py-3 text-[11px] leading-relaxed text-text-3" key={note}>
                {note}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
