import { useState } from "react";
import { ConfirmDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import { SkillEffectsText } from "./SkillListPanel.js";
import type { SkillDetailPanelProps } from "./types.js";

export function SkillDetailPanel({
  selected,
  selectedContent,
  pendingId,
  onContent,
  onSave,
  onToggle,
  onDelete,
  onTransitionSkill,
}: SkillDetailPanelProps) {
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  if (!selected) {
    return (
      <div className="flex flex-col border border-line rounded-xl overflow-hidden min-w-0">
        <OfflinePanel
          title="No skill selected."
          action="Select a local skill to inspect its body."
        />
      </div>
    );
  }
  const selectedPending =
    pendingId === `read:${selected.slug}` ||
    pendingId === `save:${selected.slug}` ||
    pendingId === `toggle:${selected.slug}` ||
    pendingId === `delete:${selected.slug}`;
  const effectsText = SkillEffectsText(selected);
  return (
    <div className="flex flex-col border border-line rounded-xl overflow-hidden min-w-0">
      <SkillDetailHeader
        selected={selected}
        selectedPending={selectedPending}
        effectsText={effectsText}
        onTransitionSkill={onTransitionSkill}
        onToggle={onToggle}
        onSave={onSave}
        onDelete={() => setConfirmDeleteOpen(true)}
      />
      <textarea
        aria-label="Skill content"
        className="flex-1 min-h-0 overflow-auto p-4 text-[12px] leading-5 text-text-2 font-mono whitespace-pre-wrap break-words bg-transparent border-0 outline-none resize-none"
        disabled={pendingId === `read:${selected.slug}`}
        value={selectedContent}
        onChange={(event) => onContent(event.target.value)}
        spellCheck={false}
      />
      {confirmDeleteOpen ? (
        <ConfirmDialog
          title="Delete skill?"
          description={`Delete skill "${selected.slug}"? This removes it from disk and can't be undone.`}
          confirmLabel="Delete"
          danger
          onCancel={() => setConfirmDeleteOpen(false)}
          onConfirm={() => {
            setConfirmDeleteOpen(false);
            onDelete();
          }}
        />
      ) : null}
    </div>
  );
}

function SkillDetailHeader({
  selected,
  selectedPending,
  effectsText,
  onTransitionSkill,
  onToggle,
  onSave,
  onDelete,
}: {
  selected: SkillItem;
  selectedPending: boolean;
  effectsText: string;
  onTransitionSkill: SkillDetailPanelProps["onTransitionSkill"];
  onToggle: () => void;
  onSave: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-4 py-3 border-b border-line bg-panel">
      <div className="min-w-0">
        <strong className="block text-[13px] font-semibold text-text truncate">
          {selected.slug}
        </strong>
        <span className="block text-[12px] text-text-3 truncate">
          {selected.disableModelInvocation ? "user-only" : selected.trigger || "model-invoked"} ·{" "}
          {selected.tags.join(", ") || "no tags"}
        </span>
        <span className="block text-[11px] text-text-3 truncate">
          {selected.lifecycle ?? "active"} · v{selected.version ?? "unversioned"} ·{" "}
          {selected.metadata?.coderight?.benchmarkIds?.join(", ") || "no benchmarks"}
        </span>
        {effectsText ? (
          <span className="block text-[11px] text-text-3 truncate">{effectsText}</span>
        ) : null}
        {selected.error ? (
          <span className="block text-[11px] text-danger truncate">{selected.error}</span>
        ) : null}
      </div>
      <div className="flex flex-wrap justify-end gap-2">
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={selectedPending}
          onClick={() => onTransitionSkill("quarantine")}
        >
          <Icon name="lock" size={13} /> Quarantine
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={selectedPending}
          onClick={() => onTransitionSkill("activate", true)}
        >
          <Icon name="play" size={13} /> Activate
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={selectedPending}
          onClick={() => onTransitionSkill("retire")}
        >
          <Icon name="pause" size={13} /> Retire
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={selectedPending}
          onClick={onToggle}
        >
          <Icon name={selected.enabled ? "pause" : "play"} size={13} />
          {selected.enabled ? "Disable" : "Enable"}
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
          disabled={selectedPending}
          onClick={onSave}
        >
          <Icon name="edit" size={13} /> Save
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1.5 h-8 px-3 rounded-lg border border-danger/50 bg-transparent text-danger text-[12px] cursor-pointer hover:bg-danger/10 disabled:opacity-40"
          disabled={selectedPending}
          onClick={onDelete}
        >
          <Icon name="trash" size={13} /> Delete
        </button>
      </div>
    </div>
  );
}
