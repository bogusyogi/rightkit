import { useCallback, useState } from "react";
import {
  createSchedule,
  deleteSchedule,
  listSchedules,
  listScheduleAdminAudit,
  patchSchedule,
  pauseSchedule,
  resumeSchedule,
  runScheduleNow,
  saveScheduleTimeZone,
} from "../../lib/daemon.js";
import type {
  DaemonConfig,
  ScheduleAdminAuditEvent,
  ScheduleRow,
  ScheduleWriteRequest,
} from "../../lib/types.js";
import { parseCommaList } from "../../lib/utils.js";

export interface ScheduleDraft {
  editingId: string | null;
  id: string;
  cronExpr: string;
  prompt: string;
  timeZone: string;
  mode: "agent" | "scriptOnly";
  workdir: string;
  toolset: string;
  executionBackend: string;
  skills: string;
  scriptAllowlist: string;
  deliveryKind: string;
  deliveryTarget: string;
  deliveryAllowPrivateNetwork: boolean;
  paused: boolean;
  maxRuntimeSeconds: string;
}

const DEFAULT_SCHEDULE_DRAFT: ScheduleDraft = {
  editingId: null,
  id: "",
  cronExpr: "0 9 * * *",
  prompt: "",
  timeZone: "UTC",
  mode: "agent",
  workdir: "",
  toolset: "cron-safe",
  executionBackend: "local",
  skills: "",
  scriptAllowlist: "",
  deliveryKind: "desktop_notification",
  deliveryTarget: "",
  deliveryAllowPrivateNetwork: false,
  paused: false,
  maxRuntimeSeconds: "",
};

interface UseSchedulesRuntimeParams {
  config: DaemonConfig | null;
  runtimeWorkspace: string;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
}

export function useSchedulesRuntime({
  config,
  runtimeWorkspace,
  onStatus,
  formatError,
}: UseSchedulesRuntimeParams) {
  const [schedules, setSchedules] = useState<ScheduleRow[]>([]);
  const [scheduleStorePath, setScheduleStorePath] = useState("");
  const [scheduleRunStorePath, setScheduleRunStorePath] = useState("");
  const [scheduleAdminAuditStorePath, setScheduleAdminAuditStorePath] = useState("");
  const [scheduleAdminAudit, setScheduleAdminAudit] = useState<ScheduleAdminAuditEvent[]>([]);
  const [scheduleActionPending, setScheduleActionPending] = useState<string | null>(null);
  const [scheduleDraft, setScheduleDraft] = useState<ScheduleDraft>(DEFAULT_SCHEDULE_DRAFT);

  const refreshSchedules = useCallback(
    async (workspaceOverride?: string) => {
      const root = (workspaceOverride || runtimeWorkspace).trim();
      if (!config || !root) {
        setSchedules([]);
        setScheduleStorePath("");
        setScheduleRunStorePath("");
        setScheduleAdminAuditStorePath("");
        setScheduleAdminAudit([]);
        return null;
      }
      const [schedulesResponse, auditResponse] = await Promise.all([
        listSchedules(config, root),
        listScheduleAdminAudit(config, root),
      ]);
      setSchedules(schedulesResponse.schedules);
      setScheduleStorePath(schedulesResponse.scheduleStorePath);
      setScheduleRunStorePath(schedulesResponse.runStorePath);
      setScheduleAdminAuditStorePath(
        auditResponse.auditStorePath || schedulesResponse.adminAuditStorePath || "",
      );
      setScheduleAdminAudit(auditResponse.history);
      return schedulesResponse;
    },
    [config, runtimeWorkspace],
  );

  const patchScheduleDraft = useCallback((patch: Partial<ScheduleDraft>) => {
    setScheduleDraft((draft) => ({ ...draft, ...patch }));
  }, []);

  const editScheduleDraft = useCallback((schedule: ScheduleRow) => {
    const delivery = schedule.delivery?.[0];
    setScheduleDraft({
      editingId: schedule.id,
      id: schedule.id,
      cronExpr: schedule.cronExpr,
      prompt: schedule.prompt,
      timeZone: schedule.timeZone || "UTC",
      mode: schedule.mode === "scriptOnly" ? "scriptOnly" : "agent",
      workdir: schedule.workdir || "",
      toolset: schedule.toolset || "",
      executionBackend: schedule.executionBackend || "local",
      skills: (schedule.skills || []).join(", "),
      scriptAllowlist: (schedule.scriptAllowlist || []).join(", "),
      deliveryKind: delivery?.kind || "none",
      deliveryTarget: delivery?.target || "",
      deliveryAllowPrivateNetwork: delivery?.allowPrivateNetwork ?? false,
      paused: schedule.paused ?? false,
      maxRuntimeSeconds:
        schedule.maxRuntimeSeconds != null ? String(schedule.maxRuntimeSeconds) : "",
    });
  }, []);

  const resetScheduleDraft = useCallback(() => {
    setScheduleDraft(DEFAULT_SCHEDULE_DRAFT);
  }, []);

  const saveScheduleDraft = useCallback(async () => {
    if (!config || !runtimeWorkspace.trim()) return;
    const cronExpr = scheduleDraft.cronExpr.trim();
    const prompt = scheduleDraft.prompt.trim();
    if (!cronExpr || !prompt) {
      onStatus("A schedule needs a cron expression and a prompt.");
      return;
    }
    const delivery =
      scheduleDraft.deliveryKind === "none"
        ? []
        : [
            {
              kind: scheduleDraft.deliveryKind,
              target: scheduleDraft.deliveryTarget.trim() || undefined,
              allowPrivateNetwork: scheduleDraft.deliveryAllowPrivateNetwork,
            },
          ];
    const parsedMaxRuntime = scheduleDraft.maxRuntimeSeconds.trim()
      ? parseInt(scheduleDraft.maxRuntimeSeconds.trim(), 10)
      : undefined;
    const body: ScheduleWriteRequest = {
      cronExpr,
      prompt,
      timeZone: scheduleDraft.timeZone.trim() || "UTC",
      mode: scheduleDraft.mode,
      workdir: scheduleDraft.workdir.trim() || null,
      toolset: scheduleDraft.toolset.trim() || null,
      executionBackend: scheduleDraft.executionBackend.trim() || null,
      skills: parseCommaList(scheduleDraft.skills),
      scriptAllowlist: parseCommaList(scheduleDraft.scriptAllowlist),
      delivery,
      paused: scheduleDraft.paused,
      maxRuntimeSeconds: parsedMaxRuntime && parsedMaxRuntime > 0 ? parsedMaxRuntime : null,
    };
    const pendingKey = scheduleDraft.editingId ? `edit:${scheduleDraft.editingId}` : "create";
    setScheduleActionPending(pendingKey);
    try {
      if (scheduleDraft.editingId) {
        await patchSchedule(config, scheduleDraft.editingId, runtimeWorkspace.trim(), body);
      } else {
        await createSchedule(config, runtimeWorkspace.trim(), {
          ...body,
          cronExpr,
          prompt,
          id: scheduleDraft.id.trim() || undefined,
        });
      }
      await refreshSchedules();
      setScheduleDraft(DEFAULT_SCHEDULE_DRAFT);
      onStatus(scheduleDraft.editingId ? "Schedule updated" : "Schedule created");
    } catch (err) {
      onStatus(
        formatError(
          err,
          scheduleDraft.editingId ? "Update schedule failed" : "Create schedule failed",
        ),
      );
    } finally {
      setScheduleActionPending(null);
    }
  }, [config, formatError, onStatus, refreshSchedules, runtimeWorkspace, scheduleDraft]);

  const removeSchedule = useCallback(
    async (id: string) => {
      if (!config || !runtimeWorkspace.trim()) return;
      setScheduleActionPending(id);
      try {
        await deleteSchedule(config, id, runtimeWorkspace.trim());
        await refreshSchedules();
        onStatus("Schedule cancelled");
      } catch (err) {
        onStatus(formatError(err, "Cancel schedule failed"));
      } finally {
        setScheduleActionPending(null);
      }
    },
    [config, formatError, onStatus, refreshSchedules, runtimeWorkspace],
  );

  const updateScheduleTimeZone = useCallback(
    async (id: string, timeZone: string) => {
      if (!config || !runtimeWorkspace.trim()) return;
      setScheduleActionPending(`tz:${id}`);
      try {
        await saveScheduleTimeZone(config, id, runtimeWorkspace.trim(), timeZone);
        await refreshSchedules();
        onStatus("Schedule timezone saved");
      } catch (err) {
        onStatus(formatError(err, "Save schedule timezone failed"));
      } finally {
        setScheduleActionPending(null);
      }
    },
    [config, formatError, onStatus, refreshSchedules, runtimeWorkspace],
  );

  const runSchedule = useCallback(
    async (id: string) => {
      if (!config || !runtimeWorkspace.trim()) return;
      setScheduleActionPending(`run:${id}`);
      try {
        await runScheduleNow(config, id, runtimeWorkspace.trim());
        await refreshSchedules();
        onStatus("Schedule run submitted");
      } catch (err) {
        onStatus(formatError(err, "Run schedule failed"));
      } finally {
        setScheduleActionPending(null);
      }
    },
    [config, formatError, onStatus, refreshSchedules, runtimeWorkspace],
  );

  const setSchedulePaused = useCallback(
    async (id: string, paused: boolean) => {
      if (!config || !runtimeWorkspace.trim()) return;
      setScheduleActionPending(`${paused ? "pause" : "resume"}:${id}`);
      try {
        if (paused) {
          await pauseSchedule(config, id, runtimeWorkspace.trim());
        } else {
          await resumeSchedule(config, id, runtimeWorkspace.trim());
        }
        await refreshSchedules();
        onStatus(paused ? "Schedule paused" : "Schedule resumed");
      } catch (err) {
        onStatus(formatError(err, paused ? "Pause schedule failed" : "Resume schedule failed"));
      } finally {
        setScheduleActionPending(null);
      }
    },
    [config, formatError, onStatus, refreshSchedules, runtimeWorkspace],
  );

  return {
    schedules,
    scheduleStorePath,
    scheduleRunStorePath,
    scheduleAdminAuditStorePath,
    scheduleAdminAudit,
    scheduleActionPending,
    scheduleDraft,
    refreshSchedules,
    patchScheduleDraft,
    editScheduleDraft,
    resetScheduleDraft,
    saveScheduleDraft,
    removeSchedule,
    updateScheduleTimeZone,
    runSchedule,
    setSchedulePaused,
  };
}
