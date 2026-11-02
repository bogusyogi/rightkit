import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { SkillItem, SkillProposalRecord } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import type { SkillProposalActionsProps } from "./types.js";

export function SkillProposalActions({
  selectedProposal,
  installedSkill,
  pendingId,
  onSelectInstalledSkill,
  onQuarantineProposal,
  onMarkBenchmark,
}: SkillProposalActionsProps) {
  if (!selectedProposal) return null;
  const pendingProposal =
    pendingId === `proposal:quarantine:${selectedProposal.id}` ||
    pendingId === `proposal:benchmark:${selectedProposal.id}`;
  return (
    <div className="flex flex-wrap justify-end gap-2">
      {installedSkill ? (
        <button
          type="button"
          className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text"
          onClick={() => onSelectInstalledSkill(installedSkill.slug)}
        >
          <Icon name="edit" size={13} /> Open
        </button>
      ) : null}
      <button
        type="button"
        className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
        disabled={pendingProposal || selectedProposal.status !== "proposed"}
        onClick={onQuarantineProposal}
      >
        <Icon name="lock" size={13} /> Quarantine
      </button>
      <button
        type="button"
        className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
        disabled={pendingProposal}
        onClick={() => onMarkBenchmark(true)}
      >
        <Icon name="check" size={13} /> Pass
      </button>
      <button
        type="button"
        className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-danger/50 bg-transparent px-3 text-[12px] text-danger cursor-pointer hover:bg-danger/10 disabled:opacity-40"
        disabled={pendingProposal}
        onClick={() => onMarkBenchmark(false)}
      >
        <Icon name="close" size={13} /> Fail
      </button>
    </div>
  );
}

export function SkillProposalList({
  proposals,
  selectedProposal,
  onSelectProposal,
}: {
  proposals: SkillProposalRecord[];
  selectedProposal: SkillProposalRecord | null;
  onSelectProposal: (id: string) => void;
}) {
  return (
    <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-line">
      <div className="flex min-h-0 flex-col divide-y divide-line overflow-auto">
        {proposals.length ? (
          proposals.map((proposal) => (
            <button
              type="button"
              key={proposal.id}
              className={`grid grid-cols-[minmax(0,1fr)_auto] gap-3 border-0 px-4 py-3 text-left cursor-pointer ${
                selectedProposal?.id === proposal.id
                  ? "bg-panel-2 text-text"
                  : "bg-transparent text-text-2 hover:bg-panel hover:text-text"
              }`}
              onClick={() => onSelectProposal(proposal.id)}
            >
              <span className="min-w-0">
                <strong className="block truncate text-[13px] font-semibold">
                  {proposal.title || proposal.slug}
                </strong>
                <span className="block truncate text-[12px] text-text-3">{proposal.summary}</span>
                <span className="block truncate text-[11px] font-mono text-text-3">
                  {proposal.contentHash}
                </span>
              </span>
              <span className="flex flex-col items-end gap-1">
                <code className="rounded border border-line bg-bg px-1.5 py-0.5 text-[10px] text-accent-2">
                  {proposal.status}
                </code>
              </span>
            </button>
          ))
        ) : (
          <OfflinePanel
            title="No skill proposals."
            action="Learning proposals will appear here after promotion."
          />
        )}
      </div>
    </div>
  );
}

export function SkillProposalEmpty({ installedSkill }: { installedSkill: SkillItem | null }) {
  if (installedSkill) return null;
  return (
    <OfflinePanel
      title="No installed skill matches the selected proposal."
      action="Install a skill to enable comparison."
    />
  );
}
