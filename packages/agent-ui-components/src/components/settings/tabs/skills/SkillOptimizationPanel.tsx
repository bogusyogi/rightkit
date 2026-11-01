import { Checkbox } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { SkillProvenanceReviewPanel } from "./SkillProvenanceReviewPanel";
import type { SkillInstallFormPanelProps } from "./types";

export function SkillInstallFormPanel({
  draft,
  pendingId,
  draftProvenanceReview,
  onDraft,
  onInstall,
  onReviewDraftProvenance,
}: SkillInstallFormPanelProps) {
  return (
    <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
      <div className="grid grid-cols-[minmax(120px,0.7fr)_minmax(140px,1fr)_minmax(180px,1.2fr)_minmax(120px,0.8fr)] gap-3">
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Slug
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.slug}
            onChange={(e) => onDraft({ ...draft, slug: e.target.value })}
            placeholder="security-check"
          />
        </label>
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Name
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.name}
            onChange={(e) => onDraft({ ...draft, name: e.target.value })}
            placeholder="Security check"
          />
        </label>
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Description
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.description}
            onChange={(e) => onDraft({ ...draft, description: e.target.value })}
            placeholder="Review risky code changes."
          />
        </label>
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Tags
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.tags}
            onChange={(e) => onDraft({ ...draft, tags: e.target.value })}
            placeholder="review,security"
          />
        </label>
      </div>
      <div className="grid grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)_auto_auto] gap-3 items-end">
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Trigger
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.trigger}
            onChange={(e) => onDraft({ ...draft, trigger: e.target.value })}
            placeholder='event == "SessionStart"'
          />
        </label>
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Source URL
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.sourceUrl}
            onChange={(e) => onDraft({ ...draft, sourceUrl: e.target.value })}
            placeholder="https://github.com/org/repo/tree/main/skills/name"
          />
        </label>
        <Checkbox checked={draft.enabled} onChange={(checked) => onDraft({ ...draft, enabled: checked })} label="Enabled" className="inline-flex items-center gap-2 h-9 text-[13px] text-text-2" />
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={
            pendingId === "install:provenance" || !draft.sourceUrl.trim() || !draft.content.trim()
          }
          onClick={onReviewDraftProvenance}
        >
          <Icon name="search" /> {pendingId === "install:provenance" ? "Reviewing" : "Review"}
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
          disabled={pendingId === "install"}
          onClick={onInstall}
        >
          <Icon name="plus" /> Install
        </button>
      </div>
      {draftProvenanceReview ? <SkillProvenanceReviewPanel review={draftProvenanceReview} /> : null}
      <label className="flex flex-col gap-1 text-[13px] text-text-2">
        Body
        <textarea
          className="min-h-[120px] px-3 py-2 rounded-lg border border-line bg-panel text-text text-[13px] leading-5 outline-none font-mono resize-y"
          value={draft.content}
          onChange={(e) => onDraft({ ...draft, content: e.target.value })}
          placeholder="# Skill body"
        />
      </label>
    </div>
  );
}
