import { Checkbox, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { ScheduleDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";

export const scheduleTimeZoneOptions = [
  "UTC",
  "UTC-12:00",
  "UTC-08:00",
  "UTC-05:00",
  "UTC+00:00",
  "UTC+01:00",
  "UTC+05:30",
  "UTC+08:00",
  "UTC+09:00",
  "UTC+10:00",
  "America/New_York",
  "America/Chicago",
  "America/Los_Angeles",
  "Europe/London",
  "Europe/Berlin",
  "Asia/Kolkata",
  "Asia/Tokyo",
  "Australia/Sydney",
];

export default function ScheduleEditor({
  draft,
  pending,
  onDraft,
  onReset,
  onSave,
}: {
  draft: ScheduleDraft;
  pending: boolean;
  onDraft: (patch: Partial<ScheduleDraft>) => void;
  onReset: () => void;
  onSave: () => void;
}) {
  const canSave = draft.cronExpr.trim().length > 0 && draft.prompt.trim().length > 0 && !pending;

  return (
    <div className="border-b border-line bg-panel px-5 py-4">
      <div className="grid grid-cols-[minmax(120px,0.7fr)_minmax(120px,0.9fr)_120px_120px] gap-3">
        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Id
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.id}
            disabled={Boolean(draft.editingId)}
            onChange={(event) => onDraft({ id: event.target.value })}
          />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Cron
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 font-mono text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.cronExpr}
            onChange={(event) => onDraft({ cronExpr: event.target.value })}
          />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Timezone
          <Select value={draft.timeZone} options={[...scheduleTimeZoneOptions.map((option) => ({ value: option, label: option }))]} onChange={(value) => onDraft({ timeZone: value })} label="Timezone" className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none" />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Mode
          <Select value={draft.mode} options={[{ value: "agent", label: "agent" }, { value: "scriptOnly", label: "scriptOnly" }]} onChange={(value) => onDraft({
                mode: value === "scriptOnly" ? "scriptOnly" : "agent",
              })} label="Mode" className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none" />
        </label>
      </div>

      <label className="mt-3 flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
        Prompt
        <textarea
          className="min-h-[74px] resize-y rounded-md border border-line bg-panel-2 px-2 py-2 text-[12px] normal-case tracking-normal text-text outline-none"
          value={draft.prompt}
          onChange={(event) => onDraft({ prompt: event.target.value })}
        />
      </label>

      <div className="mt-3 grid grid-cols-[repeat(3,minmax(0,1fr))] gap-3">
        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Workdir
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.workdir}
            onChange={(event) => onDraft({ workdir: event.target.value })}
          />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Toolset
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.toolset}
            onChange={(event) => onDraft({ toolset: event.target.value })}
          />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Backend
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.executionBackend}
            onChange={(event) => onDraft({ executionBackend: event.target.value })}
          />
        </label>
      </div>

      <div className="mt-3 grid grid-cols-[repeat(2,minmax(0,1fr))] gap-3">
        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Skills
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.skills}
            onChange={(event) => onDraft({ skills: event.target.value })}
          />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Script allowlist
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.scriptAllowlist}
            onChange={(event) => onDraft({ scriptAllowlist: event.target.value })}
          />
        </label>
      </div>

      <div className="mt-3 grid grid-cols-[160px_minmax(0,1fr)_auto_auto_auto_auto] items-end gap-3">
        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Delivery
          <Select value={draft.deliveryKind} options={[{ value: "none", label: "none" }, { value: "desktop_notification", label: "desktop_notification" }, { value: "file_artifact", label: "file_artifact" }, { value: "session_message", label: "session_message" }, { value: "webhook", label: "webhook" }, { value: "ntfy", label: "ntfy" }, { value: "telegram_chat", label: "telegram_chat" }]} onChange={(value) => onDraft({ deliveryKind: value })} label="Delivery" className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none" />
        </label>

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Target
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            value={draft.deliveryTarget}
            disabled={draft.deliveryKind === "none"}
            onChange={(event) => onDraft({ deliveryTarget: event.target.value })}
          />
        </label>

        <Checkbox
          checked={draft.deliveryAllowPrivateNetwork}
          disabled={draft.deliveryKind === "none"}
          onChange={(checked) => onDraft({ deliveryAllowPrivateNetwork: checked })}
          label="Private"
          className="flex h-8 items-center gap-2 text-[12px] text-text-2"
        />

        <Checkbox checked={draft.paused} onChange={(checked) => onDraft({ paused: checked })} label="Paused" className="flex h-8 items-center gap-2 text-[12px] text-text-2" />

        <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Max runtime (s)
          <input
            className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none"
            type="number"
            min={0}
            placeholder="unlimited"
            value={draft.maxRuntimeSeconds}
            disabled={draft.mode !== "scriptOnly"}
            onChange={(event) => onDraft({ maxRuntimeSeconds: event.target.value })}
          />
        </label>

        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            className="inline-flex h-8 items-center justify-center rounded-md border border-line bg-panel-2 px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-50"
            disabled={pending}
            onClick={onReset}
          >
            Reset
          </button>

          <button
            type="button"
            className="inline-flex h-8 items-center justify-center gap-2 rounded-md border border-accent/60 bg-accent/15 px-3 text-[12px] text-accent hover:bg-accent/20 disabled:opacity-50"
            disabled={!canSave}
            onClick={onSave}
          >
            <Icon name="check" size={14} />
            {draft.editingId ? "Update" : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
