import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  SkillGovernanceResponse,
  SkillItem,
  SkillProvenanceReviewResponse,
} from "@rightkit/agent-ui-core/lib/types";
import type { SkillInstallDraft } from "../../types.js";
import { SkillDetailPanel } from "./SkillDetailPanel.js";
import { SkillGovernancePanel } from "./SkillGovernancePanel.js";
import { SkillInstallFormPanel } from "./SkillOptimizationPanel.js";
import { SkillListPanel } from "./SkillListPanel.js";
import { SkillProposalActions } from "./SkillProposalActions.js";
import type { SkillLifecycleAction, SkillsView } from "./types.js";

export function SkillsTabHeader({
  skillDirs,
  skillCount,
  proposalCount,
  trainingRunCount,
  view,
  onChangeView,
  onRefresh,
}: {
  skillDirs: string[];
  skillCount: number;
  proposalCount: number;
  trainingRunCount: number;
  view: SkillsView;
  onChangeView: (view: SkillsView) => void;
  onRefresh: () => void;
}) {
  const installedLabel = `${skillCount} installed`;
  return (
    <div className="flex flex-wrap items-center gap-3 rounded-xl border border-line bg-panel px-4 py-3">
      <div className="flex min-w-[180px] flex-1 flex-col gap-1">
        <div className="flex min-w-0 items-center gap-2">
          <h3 className="min-w-0 truncate text-[16px] font-bold text-text">
            Composer skill library
          </h3>
          <span
            className="shrink-0 rounded-full border border-line bg-bg px-2 py-0.5 text-[11px] text-text-3"
            title={installedLabel}
          >
            {installedLabel}
          </span>
        </div>
        <p className="text-[12px] text-text-3 truncate">
          {skillDirs.join(" · ") || "Skill directories will appear after the engine connects."}
        </p>
      </div>
      <div className="inline-flex h-9 shrink-0 rounded-lg border border-line bg-panel p-1">
        <button
          type="button"
          className={`inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 text-[12px] ${
            view === "installed" ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"
          }`}
          onClick={() => onChangeView("installed")}
        >
          <Icon name="spark" size={13} /> Installed
          <span className="text-[10px] text-text-3">{skillCount}</span>
        </button>
        <button
          type="button"
          className={`inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 text-[12px] ${
            view === "proposals" ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"
          }`}
          onClick={() => onChangeView("proposals")}
        >
          <Icon name="activity" size={13} /> Proposals
          <span className="text-[10px] text-text-3">{proposalCount}</span>
        </button>
        <button
          type="button"
          className={`inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-3 text-[12px] ${
            view === "training" ? "bg-panel-2 text-text" : "text-text-3 hover:text-text"
          }`}
          onClick={() => onChangeView("training")}
        >
          <Icon name="play" size={13} /> Training
          <span className="text-[10px] text-text-3">{trainingRunCount}</span>
        </button>
      </div>
      <button
        type="button"
        className="inline-flex h-9 shrink-0 items-center gap-2 whitespace-nowrap rounded-lg border border-line bg-transparent px-4 text-[13px] text-text-2 cursor-pointer hover:bg-panel hover:text-text"
        onClick={onRefresh}
      >
        <Icon name="refresh" /> Refresh
      </button>
    </div>
  );
}

export function SkillsProposalsView({
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
}: {
  proposals: SkillGovernanceResponse["proposals"];
  versions: SkillGovernanceResponse["versions"];
  verdicts: SkillGovernanceResponse["benchmarkVerdicts"];
  selectedProposal: SkillGovernanceResponse["proposals"][number] | null;
  installedSkill: SkillItem | null;
  installedContent: string;
  pendingId: string | null;
  onSelectProposal: (id: string) => void;
  onSelectInstalledSkill: (slug: string) => void;
  onQuarantineProposal: () => void;
  onMarkBenchmark: (passed: boolean) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {selectedProposal ? (
        <SkillProposalActions
          selectedProposal={selectedProposal}
          installedSkill={installedSkill}
          pendingId={pendingId}
          onSelectInstalledSkill={onSelectInstalledSkill}
          onQuarantineProposal={onQuarantineProposal}
          onMarkBenchmark={onMarkBenchmark}
        />
      ) : null}
      <SkillGovernancePanel
        proposals={proposals}
        versions={versions}
        verdicts={verdicts}
        selectedProposal={selectedProposal}
        installedSkill={installedSkill}
        installedContent={installedContent}
        pendingId={pendingId}
        onSelectProposal={onSelectProposal}
        onSelectInstalledSkill={onSelectInstalledSkill}
        onQuarantineProposal={onQuarantineProposal}
        onMarkBenchmark={onMarkBenchmark}
      />
    </div>
  );
}

export function SkillsInstalledView({
  skills,
  selectedSlug,
  selected,
  selectedContent,
  pendingId,
  draft,
  draftProvenanceReview,
  onSelect,
  onContent,
  onSave,
  onToggle,
  onDelete,
  onTransitionSkill,
  onDraft,
  onInstall,
  onReviewDraftProvenance,
}: {
  skills: SkillItem[];
  selectedSlug: string | null;
  selected: SkillItem | null;
  selectedContent: string;
  pendingId: string | null;
  draft: SkillInstallDraft;
  draftProvenanceReview: SkillProvenanceReviewResponse | null;
  onSelect: (slug: string) => void;
  onContent: (content: string) => void;
  onSave: () => void;
  onToggle: () => void;
  onDelete: () => void;
  onTransitionSkill: (action: SkillLifecycleAction, operatorOverride?: boolean) => void;
  onDraft: (draft: SkillInstallDraft) => void;
  onInstall: () => void;
  onReviewDraftProvenance: () => void;
}) {
  const [showInstallForm, setShowInstallForm] = useState(false);
  const hasInstalledSkills = skills.length > 0;

  return (
    <div className="flex flex-col gap-5">
      {hasInstalledSkills ? (
        <div className="grid grid-cols-[minmax(260px,0.8fr)_minmax(0,1.2fr)] gap-4 min-h-[340px]">
          <SkillListPanel skills={skills} selectedSlug={selectedSlug} onSelect={onSelect} />
          <SkillDetailPanel
            selected={selected}
            selectedContent={selectedContent}
            pendingId={pendingId}
            onContent={onContent}
            onSave={onSave}
            onToggle={onToggle}
            onDelete={onDelete}
            onTransitionSkill={onTransitionSkill}
          />
        </div>
      ) : (
        <div className="cr-panel-reveal overflow-hidden rounded-xl border border-line bg-panel">
          <div className="flex min-h-[58px] items-center gap-3 border-b border-line px-4 py-3">
            <div className="min-w-0 flex-1">
              <strong className="block text-[13px] font-semibold text-text">
                Installed skills
              </strong>
              <span className="block truncate text-[12px] text-text-2">
                0 active · composer mentions become available after install
              </span>
            </div>
            <button
              type="button"
              className={`inline-flex h-9 items-center gap-2 rounded-lg px-4 text-[13px] font-semibold cursor-pointer ${
                showInstallForm
                  ? "border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                  : "border-0 bg-accent text-on-accent hover:opacity-90"
              }`}
              onClick={() => setShowInstallForm((open) => !open)}
            >
              <Icon name={showInstallForm ? "close" : "plus"} size={13} />
              {showInstallForm ? "Close new skill" : "New skill"}
            </button>
          </div>
          <div className="grid min-h-[176px] gap-4 px-4 py-5 xl:grid-cols-[minmax(0,1fr)_minmax(280px,0.7fr)]">
            <div className="grid gap-4 rounded-lg border border-[color-mix(in_srgb,var(--color-accent)_24%,var(--color-line))] bg-[color-mix(in_srgb,var(--color-accent)_4%,var(--color-bg))] px-4 py-4">
              <div className="flex items-start gap-3">
                <span className="grid h-9 w-9 flex-shrink-0 place-items-center rounded-lg border border-[color-mix(in_srgb,var(--color-accent)_32%,var(--color-line))] bg-[color-mix(in_srgb,var(--color-accent)_8%,transparent)] text-accent">
                  <Icon name="spark" size={15} />
                </span>
                <div className="min-w-0">
                  <h4 className="text-[14px] font-bold text-text">No skills installed</h4>
                  <p className="mt-1 max-w-[560px] text-[13px] leading-[1.5] text-text-2">
                    Add a local markdown skill and it becomes searchable from the composer as an
                    `@skill` mention with lifecycle, trigger, and provenance details here.
                  </p>
                  <p className="mt-2 text-[12px] text-text-3">
                    Use the New skill action above to add the first one.
                  </p>
                </div>
              </div>
              <div className="grid gap-2 sm:grid-cols-2">
                <SkillEmptyStep
                  title="Review before use"
                  detail="Trigger, tools, lifecycle, and source stay visible."
                />
                <SkillEmptyStep
                  title="Insert from composer"
                  detail="Pick installed skills from the Skills menu."
                />
              </div>
            </div>
            <div className="rounded-lg border border-line bg-bg/35 p-3">
              <span className="block text-[10px] font-semibold uppercase text-text-3">
                Composer mention preview
              </span>
              <div className="mt-2 rounded-lg border border-line bg-panel px-3 py-2">
                <div className="flex items-center gap-2 text-[12px] text-text-2">
                  <Icon name="spark" size={13} />
                  <span className="font-mono text-text">@security-check</span>
                  <span className="ml-auto rounded-full border border-line bg-bg px-1.5 py-0.5 text-[10px] uppercase text-text-3">
                    active
                  </span>
                </div>
                <p className="mt-1 text-[11px] leading-[1.45] text-text-3">
                  Installed skills appear here with trigger, scope, tools, and source before you
                  insert them.
                </p>
              </div>
              <div className="mt-3 grid gap-1.5 text-[11px] text-text-3">
                <span className="flex items-center gap-2">
                  <Icon name="activity" size={12} /> Trigger and lifecycle stay visible.
                </span>
                <span className="flex items-center gap-2">
                  <Icon name="folder" size={12} /> Source path is inspectable before use.
                </span>
              </div>
            </div>
          </div>
        </div>
      )}
      {hasInstalledSkills ? (
        <div className="flex items-center gap-3 rounded-xl border border-line bg-panel px-4 py-3">
          <div className="min-w-0 flex-1">
            <strong className="block text-[13px] font-semibold text-text">Skill creation</strong>
            <span className="block truncate text-[12px] text-text-3">
              Install a local markdown skill with provenance review when needed.
            </span>
          </div>
          <button
            type="button"
            className={`inline-flex h-9 items-center gap-2 rounded-lg px-4 text-[13px] font-semibold cursor-pointer disabled:opacity-40 ${
              showInstallForm
                ? "border border-line bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                : "border-0 bg-accent text-on-accent hover:opacity-90"
            }`}
            onClick={() => setShowInstallForm((open) => !open)}
          >
            <Icon name={showInstallForm ? "close" : "plus"} size={13} />
            {showInstallForm ? "Close" : "New skill"}
          </button>
        </div>
      ) : null}
      {showInstallForm ? (
        <div className="cr-panel-reveal">
          <SkillInstallFormPanel
            draft={draft}
            pendingId={pendingId}
            draftProvenanceReview={draftProvenanceReview}
            onDraft={onDraft}
            onInstall={onInstall}
            onReviewDraftProvenance={onReviewDraftProvenance}
          />
        </div>
      ) : null}
    </div>
  );
}

function SkillEmptyStep({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="min-w-0 rounded-lg border border-line bg-panel px-3 py-2">
      <strong className="block text-[12px] font-semibold text-text">{title}</strong>
      <span className="mt-0.5 block text-[11px] leading-[1.4] text-text-3">{detail}</span>
    </div>
  );
}
