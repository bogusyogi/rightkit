import { Checkbox, ModalShell, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { InstalledRecipe, RecipeScheduleDraft } from "@rightkit/agent-ui-core/lib/types";

const DELIVERY_OPTIONS: ReadonlyArray<{ value: RecipeScheduleDraft["deliveryKind"]; label: string }> = [
  { value: "desktop_notification", label: "desktop_notification" },
  { value: "file_artifact", label: "file_artifact" },
  { value: "session_message", label: "session_message" },
  { value: "webhook", label: "webhook" },
  { value: "ntfy", label: "ntfy" },
  { value: "telegram_chat", label: "telegram_chat" },
  { value: "none", label: "none" },
];

export default function RecipeScheduleDialog({
  draft,
  recipe,
  pending,
  onClose,
  onDraft,
  onSubmit,
}: {
  draft: RecipeScheduleDraft;
  recipe: InstalledRecipe | null;
  pending: boolean;
  onClose: () => void;
  onDraft: (patch: Partial<RecipeScheduleDraft>) => void;
  onSubmit: () => void;
}) {
  // Dialog semantics, Escape, backdrop dismissal, focus trap and focus restore are RightKit's ModalShell.
  return (
    <ModalShell
      label="Schedule recipe"
      onClose={onClose}
      width={720}
      className="overflow-hidden rounded p-0"
    >
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
          <div className="min-w-0">
            <h3 className="truncate text-[14px] font-semibold text-text">Schedule recipe</h3>
            <p className="truncate text-[12px] text-text-3">
              {recipe ? recipe.title : "No recipe selected"}
            </p>
          </div>
          <button
            type="button"
            className="inline-flex h-8 w-8 items-center justify-center rounded border border-line text-text-3 hover:text-text"
            onClick={onClose}
            aria-label="Close recipe schedule dialog"
          >
            <Icon name="close" size={14} />
          </button>
        </div>
        <div className="grid grid-cols-1 gap-3 p-4 md:grid-cols-2">
          <RecipeScheduleInput
            label="Schedule id"
            value={draft.id}
            onChange={(id) => onDraft({ id })}
            placeholder="auto"
          />
          <RecipeScheduleInput
            label="Cron"
            value={draft.cronExpr}
            onChange={(cronExpr) => onDraft({ cronExpr })}
            placeholder="0 9 * * 1-5"
          />
          <RecipeScheduleInput
            label="Time zone"
            value={draft.timeZone}
            onChange={(timeZone) => onDraft({ timeZone })}
            placeholder="UTC"
          />
          <RecipeScheduleInput
            label="Workspace"
            value={draft.workspaceRoot}
            onChange={(workspaceRoot) => onDraft({ workspaceRoot })}
            placeholder="workspace root"
          />
          <RecipeScheduleInput
            label="Workdir"
            value={draft.workdir}
            onChange={(workdir) => onDraft({ workdir })}
            placeholder="optional absolute path"
          />
          <RecipeScheduleInput
            label="Toolset"
            value={draft.toolset}
            onChange={(toolset) => onDraft({ toolset })}
            placeholder="recipe default"
          />
          <RecipeScheduleInput
            label="Execution backend"
            value={draft.executionBackend}
            onChange={(executionBackend) => onDraft({ executionBackend })}
            placeholder="local"
          />
          <RecipeScheduleInput
            label="Skills"
            value={draft.skills}
            onChange={(skills) => onDraft({ skills })}
            placeholder="comma separated"
          />
          <div className="flex min-w-0 flex-col gap-1 text-[12px] text-text-3">
            <span>Delivery</span>
            <Select<RecipeScheduleDraft["deliveryKind"]>
              label="Delivery"
              value={draft.deliveryKind}
              onChange={(deliveryKind) => onDraft({ deliveryKind })}
              options={DELIVERY_OPTIONS}
            />
          </div>
          <RecipeScheduleInput
            label="Delivery target"
            value={draft.deliveryTarget}
            onChange={(deliveryTarget) => onDraft({ deliveryTarget })}
            placeholder="optional"
          />
          <div className="flex h-9 items-center gap-2 self-end text-[12px] text-text-2">
            <Checkbox
              label="Paused"
              checked={draft.paused}
              onChange={(paused) => onDraft({ paused })}
            />
          </div>
        </div>
        <div className="flex items-center justify-end gap-2 border-t border-line px-4 py-3">
          <button
            type="button"
            className="inline-flex h-8 items-center gap-2 rounded border border-line px-3 text-[12px] text-text-2 hover:text-text"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            type="button"
            className="inline-flex h-8 items-center gap-2 rounded border border-accent/50 bg-accent/10 px-3 text-[12px] font-semibold text-accent hover:bg-accent/15 disabled:opacity-40"
            disabled={pending || !recipe || !draft.cronExpr.trim()}
            onClick={onSubmit}
          >
            <Icon name="flag" size={13} />
            {pending ? "Scheduling" : "Schedule"}
          </button>
        </div>
    </ModalShell>
  );
}

function RecipeScheduleInput({
  label,
  value,
  placeholder,
  onChange,
}: {
  label: string;
  value: string;
  placeholder?: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex min-w-0 flex-col gap-1 text-[12px] text-text-3">
      {label}
      <input
        className="h-9 rounded border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
        value={value}
        placeholder={placeholder}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  );
}
