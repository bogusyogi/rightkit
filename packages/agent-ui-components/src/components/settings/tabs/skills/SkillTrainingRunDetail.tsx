import { Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { BenchmarkGateKind, SkillOptimizationRun } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import { SkillProposalMeta } from "@rightkit/agent-ui-settings/components/settings/tabs/skills/SkillShared";
import type { SkillTrainingPanelProps } from "./types.js";

export default function SkillTrainingRunDetail({
  selectedRun,
  epochDraft,
  executePending,
  proposePending,
  onEpochDraft,
  onProposeCandidate,
  onExecuteEpoch,
}: {
  selectedRun: SkillOptimizationRun | null;
  epochDraft: SkillTrainingPanelProps["epochDraft"];
  executePending: boolean;
  proposePending: boolean;
  onEpochDraft: SkillTrainingPanelProps["onEpochDraft"];
  onProposeCandidate: SkillTrainingPanelProps["onProposeCandidate"];
  onExecuteEpoch: SkillTrainingPanelProps["onExecuteEpoch"];
}) {
  if (!selectedRun) {
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-line">
        <OfflinePanel
          title="No training run selected."
          action="Create or select a run to execute validation-gated candidate edits."
        />
      </div>
    );
  }
  const latestCandidate = selectedRun.candidates.at(-1) ?? null;
  const latestProposal = selectedRun.pendingCandidateProposals.at(-1) ?? null;
  const acceptedCount = selectedRun.candidates.filter((c) => c.accepted).length;
  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-line">
      <div className="border-b border-line bg-panel px-4 py-3">
        <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
          <div className="min-w-0">
            <strong className="block truncate text-[13px] font-semibold text-text">
              {selectedRun.name}
            </strong>
            <span className="block truncate text-[11px] font-mono text-accent-2">
              {selectedRun.status} · epoch {selectedRun.currentEpoch} ·{" "}
              {selectedRun.bestVersionHash ?? "no best hash"}
            </span>
            <span className="block truncate text-[11px] text-text-3">
              {selectedRun.target.optimizer} optimizes {selectedRun.target.skillId} for{" "}
              {selectedRun.target.targetModel || "session/default"}
            </span>
          </div>
          <div className="grid grid-cols-3 gap-2 text-right">
            <SkillProposalMeta
              label="Score"
              value={selectedRun.skillBundle.verificationScore.toFixed(2)}
            />
            <SkillProposalMeta label="Accepted" value={acceptedCount.toString()} />
            <SkillProposalMeta
              label="Rejected"
              value={selectedRun.rejectedEdits.length.toString()}
            />
          </div>
        </div>
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_minmax(220px,0.65fr)] divide-x divide-line overflow-hidden">
        <div className="flex min-w-0 flex-col gap-3 overflow-auto p-4">
          <label className="flex flex-col gap-1 text-[12px] text-text-2">
            Candidate skill body
            <textarea
              className="min-h-[220px] resize-y rounded-lg border border-line bg-panel px-3 py-2 font-mono text-[12px] leading-5 text-text outline-none"
              value={epochDraft.proposedBody}
              onChange={(event) =>
                onEpochDraft({ ...epochDraft, proposedBody: event.target.value })
              }
              placeholder="# Candidate skill body"
            />
          </label>
          <div className="grid grid-cols-[110px_120px_minmax(0,1fr)_110px] gap-2">
            <label className="flex flex-col gap-1 text-[12px] text-text-2">
              Score
              <input
                className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
                value={epochDraft.validationScore}
                onChange={(event) =>
                  onEpochDraft({
                    ...epochDraft,
                    validationScore: event.target.value,
                  })
                }
                placeholder="0.92"
              />
            </label>
            <label className="flex flex-col gap-1 text-[12px] text-text-2">
              Gate mode
              <Select value={epochDraft.gateMode} options={[{ value: "", label: "all" }, { value: "hard", label: "hard" }, { value: "soft", label: "soft" }, { value: "mixed", label: "mixed" }]} onChange={(value) => onEpochDraft({
                    ...epochDraft,
                    gateMode: value as "" | BenchmarkGateKind,
                  })} label="Gate mode" className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none" />
            </label>
            <label className="flex flex-col gap-1 text-[12px] text-text-2">
              Summary
              <input
                className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
                value={epochDraft.summary}
                onChange={(event) => onEpochDraft({ ...epochDraft, summary: event.target.value })}
                placeholder="Tighten evidence rule"
              />
            </label>
            <label className="flex flex-col gap-1 text-[12px] text-text-2">
              Threshold
              <input
                className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none"
                value={epochDraft.threshold}
                onChange={(event) => onEpochDraft({ ...epochDraft, threshold: event.target.value })}
                placeholder="0.9"
              />
            </label>
          </div>
          <div className="flex items-center justify-end gap-2">
            <button
              type="button"
              className="inline-flex h-9 items-center gap-2 rounded-lg border border-line bg-transparent px-4 text-[13px] font-semibold text-text-2 cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
              disabled={!selectedRun || proposePending}
              onClick={onProposeCandidate}
            >
              <Icon name="spark" /> {proposePending ? "Generating" : "Generate candidate"}
            </button>
            <button
              type="button"
              className="inline-flex h-9 items-center gap-2 rounded-lg border-0 bg-accent px-4 text-[13px] font-semibold text-on-accent cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
              disabled={executePending || !epochDraft.proposedBody.trim()}
              onClick={onExecuteEpoch}
            >
              <Icon name="play" /> {executePending ? "Running" : "Run epoch"}
            </button>
          </div>
        </div>
        <SkillTrainingEvidence
          latestCandidate={latestCandidate}
          latestProposal={latestProposal}
          selectedRun={selectedRun}
        />
      </div>
    </div>
  );
}

function SkillTrainingEvidence({
  latestCandidate,
  latestProposal,
  selectedRun,
}: {
  latestCandidate: SkillOptimizationRun["candidates"][number] | null;
  latestProposal: SkillOptimizationRun["pendingCandidateProposals"][number] | null;
  selectedRun: SkillOptimizationRun;
}) {
  return (
    <div className="flex min-w-0 flex-col overflow-auto p-4">
      <h4 className="text-[12px] font-semibold text-text">Candidate evidence</h4>
      <div className="mt-3 flex flex-col gap-2 text-[11px] text-text-3">
        {latestCandidate ? (
          <>
            <code className="rounded border border-line bg-bg px-2 py-1">
              latest {latestCandidate.id}
            </code>
            <code
              className={`rounded border border-line bg-bg px-2 py-1 ${
                latestCandidate.accepted ? "text-accent-2" : "text-danger"
              }`}
            >
              {latestCandidate.accepted ? "accepted" : "rejected"} ·{" "}
              {latestCandidate.validationScore.toFixed(2)}
            </code>
            <code className="rounded border border-line bg-bg px-2 py-1">
              {latestCandidate.proposedHash}
            </code>
          </>
        ) : (
          <span>No candidate epochs recorded yet.</span>
        )}
        {latestProposal ? (
          <>
            <code className="rounded border border-line bg-bg px-2 py-1">
              proposal {latestProposal.id}
            </code>
            <code className="rounded border border-line bg-bg px-2 py-1">
              {latestProposal.proposedHash}
            </code>
            <code className="rounded border border-line bg-bg px-2 py-1">
              {latestProposal.source} · meta {latestProposal.metaSkillHash}
            </code>
          </>
        ) : null}
        <code className="rounded border border-line bg-bg px-2 py-1 text-accent-2">
          meta-skill v{selectedRun.metaSkill.version} · {selectedRun.metaSkill.contentHash}
        </code>
        <div className="rounded border border-line bg-panel p-2">
          <strong className="block text-[11px] text-text">Meta-skill guidance</strong>
          <span className="mt-1 block whitespace-pre-wrap text-[11px] text-text-3">
            {selectedRun.metaSkill.guidance.slice(-3).join("\n")}
          </span>
        </div>
        {selectedRun.history.slice(-6).map((entry) => (
          <div
            key={`${entry.event}-${entry.epoch}-${entry.createdAt}`}
            className="rounded border border-line bg-panel p-2"
          >
            <strong className="block text-[11px] text-text">
              {entry.event} · epoch {entry.epoch}
            </strong>
            <span className="block text-[11px] text-text-3">{entry.message}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
