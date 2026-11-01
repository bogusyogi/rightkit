import { EmptyState } from "@rightkit/react-shell/react";
import { useState } from "react";
import type { ScheduleAdminAuditEvent, ScheduleRow } from "@rightkit/agent-ui-core/lib/types";
import { ConfirmDialog, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import ScheduleEditor, { scheduleTimeZoneOptions } from "./ScheduleEditor";
import type { ScheduleDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";
import {
  formatEpochMinute,
  formatScheduleAdminAuditAt,
  formatScheduleAdminAuditDetails,
  formatScheduleLastClaimed,
  formatScheduleMetadata,
  formatScheduleNextDue,
  formatScheduleRun,
  formatScheduleRunContext,
} from "./runtimeUtils";

export default function SchedulesRuntimePanel({
  schedules,
  scheduleStorePath,
  scheduleRunStorePath,
  scheduleAdminAuditStorePath,
  scheduleAdminAudit,
  pendingId,
  draft,
  onDelete,
  onDraft,
  onEdit,
  onResetDraft,
  onSave,
  onTimeZone,
  onRun,
  onPaused,
}: {
  schedules: ScheduleRow[];
  scheduleStorePath: string;
  scheduleRunStorePath: string;
  scheduleAdminAuditStorePath: string;
  scheduleAdminAudit: ScheduleAdminAuditEvent[];
  pendingId: string | null;
  draft: ScheduleDraft;
  onDelete: (id: string) => void;
  onDraft: (patch: Partial<ScheduleDraft>) => void;
  onEdit: (schedule: ScheduleRow) => void;
  onResetDraft: () => void;
  onSave: () => void;
  onTimeZone: (id: string, timeZone: string) => void;
  onRun: (id: string) => void;
  onPaused: (id: string, paused: boolean) => void;
}) {
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <ScheduleEditor
        draft={draft}
        pending={pendingId === "create" || pendingId === `edit:${draft.editingId}`}
        onDraft={onDraft}
        onReset={onResetDraft}
        onSave={onSave}
      />

      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)] gap-3 px-5 py-3 border-b border-line text-[12px] text-text-3">
        <span className="truncate">{scheduleStorePath}</span>
        <span className="truncate">{scheduleRunStorePath}</span>
        <span className="truncate">{scheduleAdminAuditStorePath}</span>
      </div>

      {scheduleAdminAudit.length ? (
        <div className="border-b border-line px-5 py-3">
          <div className="mb-2 flex items-center justify-between gap-3">
            <strong className="text-[12px] font-semibold uppercase tracking-wide text-text-2">
              Admin audit
            </strong>
            <span className="text-[11px] text-text-3">{scheduleAdminAudit.length} events</span>
          </div>

          <div className="grid grid-cols-[120px_minmax(0,0.9fr)_minmax(0,1fr)_minmax(0,1.3fr)] gap-3 text-[11px] text-text-3">
            {scheduleAdminAudit.slice(0, 6).map((event) => (
              <div className="contents" key={event.id}>
                <code className="font-mono text-accent-2 truncate">{event.action}</code>
                <span className="truncate" title={event.scheduleId}>
                  {event.scheduleId}
                </span>
                <span className="truncate">
                  {event.status} - {event.actor}
                </span>
                <span className="truncate" title={formatScheduleAdminAuditDetails(event)}>
                  {formatScheduleAdminAuditAt(event.createdAt)} -{" "}
                  {formatScheduleAdminAuditDetails(event)}
                </span>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      {schedules.length === 0 ? (
        <EmptyState icon={<Icon name="branch" size={18} />} title="No schedules">{"Workspace schedules appear here after `schedule_cron` creates them. Use the editor above to register the first cron-triggered run."}</EmptyState>
      ) : (
        <div className="flex flex-col divide-y divide-line">
          {schedules.map((schedule) => {
            const timeZone = schedule.timeZone || "UTC";
            const timeZoneOptions = scheduleTimeZoneOptions.includes(timeZone)
              ? scheduleTimeZoneOptions
              : [timeZone, ...scheduleTimeZoneOptions];

            return (
              <div
                className="grid grid-cols-[minmax(0,1.2fr)_minmax(180px,0.8fr)_120px_auto] gap-4 px-5 py-3 items-center"
                key={schedule.id}
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-2 min-w-0">
                    <strong className="block text-[13px] font-semibold text-text truncate">
                      {schedule.id}
                    </strong>

                    {schedule.paused ? (
                      <span className="shrink-0 rounded border border-warning/40 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-warning">
                        Paused
                      </span>
                    ) : null}
                  </div>

                  <p className="text-[12px] text-text-3 truncate">{schedule.prompt}</p>
                  <p className="text-[11px] text-text-3 truncate">
                    {formatScheduleMetadata(schedule)}
                  </p>
                </div>

                <div className="min-w-0 text-right">
                  <code className="block text-[12px] font-mono text-accent-2 truncate">
                    {schedule.cronExpr} - {timeZone}
                  </code>

                  <span className="block text-[11px] text-text-3 truncate">
                    {formatScheduleNextDue(schedule)}
                  </span>
                  <span className="block text-[11px] text-text-3 truncate">
                    {formatScheduleLastClaimed(schedule.lastClaimedMinute)}
                  </span>

                  <span
                    className="block text-[11px] text-text-3 truncate"
                    title={formatScheduleRunContext(schedule.latestRun)}
                  >
                    {formatScheduleRun(schedule.latestRun)}
                  </span>

                  {schedule.latestRun ? (
                    <span className="block text-[11px] text-text-3 truncate">
                      {formatScheduleRunContext(schedule.latestRun)}
                    </span>
                  ) : null}
                </div>

                <Select
                  value={timeZone}
                  disabled={pendingId === `tz:${schedule.id}`}
                  onChange={(next) => onTimeZone(schedule.id, next)}
                  label="Schedule timezone"
                  options={timeZoneOptions.map((option) => ({ value: option, label: option }))}
                />

                <div className="flex items-center justify-end gap-1">
                  <button
                    type="button"
                    className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-accent hover:border-accent cursor-pointer disabled:opacity-50"
                    disabled={pendingId === `edit:${schedule.id}`}
                    onClick={() => onEdit(schedule)}
                    title="Edit schedule"
                    aria-label="Edit schedule"
                  >
                    <Icon name="edit" size={14} />
                  </button>

                  <button
                    type="button"
                    className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-accent hover:border-accent cursor-pointer disabled:opacity-50"
                    disabled={pendingId === `run:${schedule.id}` || schedule.paused}
                    onClick={() => onRun(schedule.id)}
                    title="Run schedule now"
                    aria-label="Run schedule now"
                  >
                    <Icon name="play" size={14} />
                  </button>

                  <button
                    type="button"
                    className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-accent hover:border-accent cursor-pointer disabled:opacity-50"
                    disabled={
                      pendingId === `pause:${schedule.id}` || pendingId === `resume:${schedule.id}`
                    }
                    onClick={() => onPaused(schedule.id, !schedule.paused)}
                    title={schedule.paused ? "Resume schedule" : "Pause schedule"}
                    aria-label={schedule.paused ? "Resume schedule" : "Pause schedule"}
                  >
                    <Icon name={schedule.paused ? "play" : "pause"} size={14} />
                  </button>

                  <button
                    type="button"
                    className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-panel text-text-2 hover:text-danger hover:border-danger cursor-pointer disabled:opacity-50"
                    disabled={pendingId === schedule.id}
                    onClick={() => setPendingDelete(schedule.id)}
                    title="Cancel schedule"
                    aria-label="Cancel schedule"
                  >
                    <Icon name="trash" size={14} />
                  </button>
                </div>

                {schedule.runs?.length ? (
                  <div className="col-span-4 min-w-0 rounded-md border border-line bg-panel-2 px-3 py-2">
                    <div className="grid grid-cols-[120px_minmax(0,0.85fr)_minmax(0,1fr)_minmax(0,1fr)] gap-3 text-[11px] text-text-3">
                      {schedule.runs.slice(0, 3).map((run) => (
                        <div className="contents" key={`${run.status}-${run.epochMinute}`}>
                          <code className="font-mono text-accent-2 truncate">{run.status}</code>
                          <span className="truncate">{formatEpochMinute(run.epochMinute)}</span>
                          <span className="truncate" title={formatScheduleRunContext(run)}>
                            {formatScheduleRunContext(run)}
                          </span>
                          <span className="truncate">{run.message || run.sessionId}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
      {pendingDelete !== null ? (
        <ConfirmDialog
          title="Cancel schedule?"
          description={`Cancel schedule "${pendingDelete ?? ""}"? It will not run again.`}
          confirmLabel="Cancel schedule"
          danger
          onCancel={() => setPendingDelete(null)}
          onConfirm={() => {
            if (pendingDelete) onDelete(pendingDelete);
            setPendingDelete(null);
          }}
        />
      ) : null}
    </div>
  );
}
