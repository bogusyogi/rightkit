import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  SkillBenchmarkVerdict,
  SkillProposalRecord,
  SkillVersionRecord,
} from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import { SkillProposalMeta, SkillProposalStat } from "@rightkit/agent-ui-settings/components/settings/tabs/skills/SkillShared";
import type { SkillGovernancePanelProps } from "./types.js";

export function SkillGovernancePanel({
  proposals,
  versions,
  verdicts,
  selectedProposal,
  installedSkill,
  installedContent,
  pendingId,
  onSelectProposal,
  onSelectInstalledSkill,
  onQuarantineProposal,
  onMarkBenchmark,
}: SkillGovernancePanelProps) {
  const proposalVerdicts = selectedProposal
    ? verdicts.filter((v) => v.proposalId === selectedProposal.id)
    : [];
  const latestVerdict = proposalVerdicts.at(-1) ?? null;
  const proposalVersions = selectedProposal
    ? versions.filter((v) => v.proposalId === selectedProposal.id)
    : [];
  const pendingProposal = selectedProposal
    ? pendingId === `proposal:quarantine:${selectedProposal.id}` ||
      pendingId === `proposal:benchmark:${selectedProposal.id}`
    : false;
  return (
    <div className="grid min-h-[520px] grid-cols-[minmax(280px,0.85fr)_minmax(0,1.15fr)] gap-4">
      <SkillGovernanceList
        proposals={proposals}
        verdicts={verdicts}
        selectedProposal={selectedProposal}
        onSelectProposal={onSelectProposal}
      />
      <div className="flex min-w-0 flex-col overflow-hidden rounded-xl border border-line">
        {selectedProposal ? (
          <>
            <div className="border-b border-line bg-panel px-4 py-3">
              <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
                <div className="min-w-0">
                  <strong className="block truncate text-[13px] font-semibold text-text">
                    {selectedProposal.slug}
                  </strong>
                  <span className="block truncate text-[11px] font-mono text-accent-2">
                    {selectedProposal.status} · {selectedProposal.contentHash}
                  </span>
                  <span className="block truncate text-[11px] text-text-3">
                    source {selectedProposal.sourceSessionId || "unknown"} /{" "}
                    {selectedProposal.sourceTaskId || "no task"}
                  </span>
                </div>
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
              </div>
            </div>
            <div className="grid grid-cols-3 gap-3 border-b border-line p-4">
              <SkillProposalMeta
                label="Latest verdict"
                value={
                  latestVerdict
                    ? `${latestVerdict.benchmarkId} ${latestVerdict.passed ? "pass" : "fail"}`
                    : "none"
                }
              />
              <SkillProposalMeta
                label="Gate"
                value={
                  latestVerdict
                    ? `${latestVerdict.gateMode} ${latestVerdict.scoreContinuous}`
                    : "none"
                }
              />
              <SkillProposalMeta label="Versions" value={proposalVersions.length.toString()} />
            </div>
            <div className="grid min-h-0 flex-1 grid-cols-2 divide-x divide-line overflow-hidden">
              <SkillProposalBodyCell
                title="Installed"
                body={installedContent || "# No installed skill body loaded"}
              />
              <SkillProposalBodyCell title="Proposed" body={selectedProposal.proposedContent} />
            </div>
            <SkillGovernanceFooter
              proposalVersions={proposalVersions}
              proposalVerdicts={proposalVerdicts}
            />
          </>
        ) : (
          <OfflinePanel
            title="No proposal selected."
            action="Select a proposal to inspect governance evidence."
          />
        )}
      </div>
    </div>
  );
}

function SkillGovernanceList({
  proposals,
  verdicts,
  selectedProposal,
  onSelectProposal,
}: {
  proposals: SkillProposalRecord[];
  verdicts: SkillBenchmarkVerdict[];
  selectedProposal: SkillProposalRecord | null;
  onSelectProposal: (id: string) => void;
}) {
  return (
    <div className="flex min-h-0 flex-col overflow-hidden rounded-xl border border-line">
      <div className="grid grid-cols-3 gap-2 border-b border-line bg-panel p-3">
        <SkillProposalStat label="Proposals" value={proposals.length} />
        <SkillProposalStat
          label="Benchmarked"
          value={proposals.filter((p) => p.status === "benchmarked").length}
        />
        <SkillProposalStat
          label="Active"
          value={proposals.filter((p) => p.status === "active").length}
        />
      </div>
      <div className="flex min-h-0 flex-col divide-y divide-line overflow-auto">
        {proposals.length ? (
          proposals.map((proposal) => (
            <SkillGovernanceRow
              key={proposal.id}
              proposal={proposal}
              latestVerdict={verdicts.filter((v) => v.proposalId === proposal.id).at(-1) ?? null}
              selected={selectedProposal?.id === proposal.id}
              onSelect={onSelectProposal}
            />
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

function SkillGovernanceRow({
  proposal,
  latestVerdict,
  selected,
  onSelect,
}: {
  proposal: SkillProposalRecord;
  latestVerdict: SkillBenchmarkVerdict | null;
  selected: boolean;
  onSelect: (id: string) => void;
}) {
  return (
    <button
      type="button"
      className={`grid grid-cols-[minmax(0,1fr)_auto] gap-3 border-0 px-4 py-3 text-left cursor-pointer ${
        selected
          ? "bg-panel-2 text-text"
          : "bg-transparent text-text-2 hover:bg-panel hover:text-text"
      }`}
      onClick={() => onSelect(proposal.id)}
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
        <code
          className={`text-[10px] ${
            latestVerdict?.passed ? "text-accent-2" : latestVerdict ? "text-danger" : "text-text-3"
          }`}
        >
          {latestVerdict
            ? `${latestVerdict.benchmarkId}:${latestVerdict.passed ? "pass" : "fail"}`
            : "no verdict"}
        </code>
      </span>
    </button>
  );
}

function SkillProposalBodyCell({ title, body }: { title: string; body: string }) {
  return (
    <div className="flex min-w-0 flex-col overflow-hidden">
      <div className="border-b border-line bg-panel px-3 py-2 text-[11px] font-semibold text-text-2">
        {title}
      </div>
      <pre className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words p-3 text-[11px] leading-5 text-text-2">
        {body}
      </pre>
    </div>
  );
}

function SkillGovernanceFooter({
  proposalVersions,
  proposalVerdicts,
}: {
  proposalVersions: SkillVersionRecord[];
  proposalVerdicts: SkillBenchmarkVerdict[];
}) {
  return (
    <div className="border-t border-line bg-panel px-4 py-3">
      <div className="flex flex-wrap gap-2 text-[11px] text-text-3">
        {proposalVersions.map((version) => (
          <code key={version.id} className="rounded border border-line bg-bg px-2 py-1">
            v{version.version} {version.contentHash}
          </code>
        ))}
        {proposalVerdicts.map((verdict) => (
          <code key={verdict.id} className="rounded border border-line bg-bg px-2 py-1">
            {verdict.benchmarkId} {verdict.passed ? "pass" : "fail"} {verdict.verdictHash}
          </code>
        ))}
      </div>
    </div>
  );
}
