import { useCallback, useEffect, useRef, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hostAppName } from "../appName.js";

export interface ComputerUsePermissionSource {
  attribution?: string;
  executable?: string;
  note?: string;
  pid?: number;
  responsiblePpid?: number;
}

export interface ComputerUseCheck {
  label: string;
  status: string;
  message: string;
}

/** Renderer mirror of the host adapter's OS-aware computer-use preflight. */
export interface ComputerUseStatus {
  platform: string;
  platformSupported: boolean;
  installed: boolean;
  version: string | null;
  ready: boolean | null;
  canGrant: boolean;
  checks: ComputerUseCheck[];
  accessibility: boolean | null;
  screenRecording: boolean | null;
  screenRecordingCapturable?: boolean | null;
  source: ComputerUsePermissionSource | null;
  error: string | null;
}

export interface ComputerUseGrantResult {
  ok: boolean;
  actionId?: string;
  error?: string;
}

export interface ComputerUseActionStatus {
  running: boolean;
}

export interface ComputerUseAdapter {
  getStatus: () => Promise<ComputerUseStatus>;
  grantPermissions?: () => Promise<ComputerUseGrantResult>;
  getActionStatus?: (actionId: string, timeoutMs?: number) => Promise<ComputerUseActionStatus>;
}

export interface ComputerUsePanelProps {
  /** The Tauri/native connector. It owns permission probing and grants. */
  adapter?: ComputerUseAdapter;
  /** Controlled status is useful while a parent owns the adapter lifecycle. */
  status?: ComputerUseStatus | null;
  loading?: boolean;
  error?: string | null;
  onRefresh?: () => void;
  onGrantPermissions?: () => Promise<ComputerUseGrantResult>;
  onConfiguredChange?: () => void;
}

const PLATFORM_NOTE: Record<string, string> = {
  linux: "Drives your desktop through the host accessibility stack; no OS permission prompt is needed.",
  win32: "The first run may trigger a Windows UIAccess prompt for the computer-use worker; allow it.",
};

function statusTone(value: boolean | null): string {
  return value === true ? "text-accent-2 border-accent-2/40" : value === false ? "text-danger border-danger/40" : "text-text-3 border-line";
}

function statusLabel(value: boolean | null): string {
  return value === true ? "Granted" : value === false ? "Not granted" : "Unknown";
}

function readinessLabel(value: boolean | null): string {
  return value === true ? "Ready" : value === false ? "Not ready" : "Unknown";
}

function PermissionRow({ granted, label, hint }: { granted: boolean | null; label: string; hint: string }) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-panel-2 p-3">
      <div className="min-w-0">
        <span className="text-[13px] font-medium text-text">{label}</span>
        <p className="mt-0.5 text-[11px] text-text-3">{hint}</p>
      </div>
      <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] ${statusTone(granted)}`}>
        <Icon name={granted === true ? "check" : granted === false ? "close" : "alert"} size={11} />
        {statusLabel(granted)}
      </span>
    </div>
  );
}

/**
 * Cross-platform computer-use readiness and permission panel. Like Hermes'
 * source panel, this keeps the slow grant flow out of render and polls the
 * host action until the operator returns from system settings.
 */
export function ComputerUsePanel({
  adapter,
  status: controlledStatus,
  loading: controlledLoading,
  error: controlledError,
  onRefresh,
  onGrantPermissions,
  onConfiguredChange,
}: ComputerUsePanelProps) {
  const [loadedStatus, setLoadedStatus] = useState<ComputerUseStatus | null>(null);
  const [loading, setLoading] = useState(controlledStatus === undefined);
  const [error, setError] = useState<string | null>(null);
  const [granting, setGranting] = useState(false);
  const activeRef = useRef(false);
  const status = controlledStatus === undefined ? loadedStatus : controlledStatus;
  const isLoading = controlledLoading ?? (controlledStatus === undefined && loading);
  const displayError = controlledError ?? error;

  const refresh = useCallback(async () => {
    if (!adapter) {
      setLoading(false);
      setError("Computer Use host adapter is not connected.");
      return;
    }
    setError(null);
    try {
      setLoadedStatus(await adapter.getStatus());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not read Computer Use status");
    } finally {
      setLoading(false);
    }
  }, [adapter]);

  useEffect(() => {
    activeRef.current = true;
    if (controlledStatus === undefined) void refresh();
    return () => {
      activeRef.current = false;
    };
  }, [controlledStatus, refresh]);

  const grant = useCallback(async () => {
    const grantPermissions = onGrantPermissions ?? adapter?.grantPermissions;
    if (!grantPermissions) {
      setError("Computer Use permission grants are not available on this host.");
      return;
    }
    setGranting(true);
    setError(null);
    try {
      const started = await grantPermissions();
      if (!started.ok) {
        setError(started.error ?? "Could not request Computer Use permissions");
        return;
      }
      if (started.actionId && adapter?.getActionStatus) {
        for (let attempt = 0; attempt < 150 && activeRef.current; attempt += 1) {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
          if (!activeRef.current) break;
          const polled = await adapter.getActionStatus(started.actionId, 200);
          if (!polled.running) break;
        }
      }
      if (activeRef.current) {
        if (controlledStatus === undefined && adapter) await refresh();
        onConfiguredChange?.();
      }
    } catch (cause) {
      if (activeRef.current) setError(cause instanceof Error ? cause.message : "Could not request permissions");
    } finally {
      if (activeRef.current) setGranting(false);
    }
  }, [adapter, controlledStatus, onConfiguredChange, onGrantPermissions, refresh]);

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 px-1 text-[12px] text-text-3" data-testid="computer-use-loading">
        <Icon name="activity" size={13} /> Checking Computer Use status…
      </div>
    );
  }

  if (!status) {
    return (
      <div className="rounded-xl border border-line bg-panel p-4 text-[12px] text-text-3" role="status">
        {displayError ?? "Computer Use status is unavailable."}
      </div>
    );
  }

  if (!status.platformSupported) {
    return <p className="px-1 text-[12px] text-text-3">Computer Use is not supported on this platform ({status.platform}).</p>;
  }

  if (!status.installed) {
    return (
      <div className="rounded-xl border border-line bg-panel p-4 text-[12px] text-text-3">
        Install the computer-use backend to drive this machine.
        {status.canGrant ? " Then grant Accessibility and Screen Recording here." : ""}
      </div>
    );
  }

  const failingChecks = status.checks.filter((check) => check.status !== "ok");
  return (
    <section className="flex flex-col gap-3 rounded-xl border border-line bg-panel p-4" aria-labelledby="computer-use-title">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h4 id="computer-use-title" className="text-[13px] font-semibold text-text">Computer Use readiness</h4>
          <p className="mt-1 text-[11px] text-text-3">
            {status.canGrant
              ? `Permissions attach to the computer-use driver identity, not ${hostAppName()}.`
              : PLATFORM_NOTE[status.platform] ?? "Readiness is reported by the host driver."}
          </p>
          {status.version ? <p className="mt-0.5 font-mono text-[10px] text-text-3">{status.version}</p> : null}
        </div>
        <button type="button" className="inline-flex items-center gap-1.5 rounded-lg border border-line px-3 py-1.5 text-[11px] text-text-2 hover:bg-panel-2" onClick={() => { onRefresh?.(); if (controlledStatus === undefined) void refresh(); }}>
          <Icon name="refresh" size={12} /> Recheck
        </button>
      </div>

      {status.canGrant ? (
        <div className="grid gap-2">
          <PermissionRow granted={status.accessibility} label="Accessibility" hint="Allows clicks, keystrokes, and accessibility-tree reads." />
          <PermissionRow granted={status.screenRecording} label="Screen Recording" hint="Allows screenshots of app windows." />
        </div>
      ) : (
        <div className="flex items-center justify-between gap-2 rounded-lg bg-panel-2 p-3">
          <span className="text-[13px] font-medium text-text">Driver health</span>
          <span className={`rounded-full border px-2 py-0.5 text-[11px] ${statusTone(status.ready)}`}>{readinessLabel(status.ready)}</span>
        </div>
      )}

      {failingChecks.map((check) => <p className="text-[11px] text-text-3" key={check.label}><Icon name="alert" size={11} /> {check.label}: {check.message}</p>)}
      {(displayError ?? status.error) ? <p className="text-[11px] text-danger"><Icon name="alert" size={11} /> {displayError ?? status.error}</p> : null}
      {status.ready ? (
        <p className="flex items-center gap-1.5 text-[12px] text-text-3"><Icon name="check" size={13} /> Computer Use is ready.</p>
      ) : status.canGrant ? (
        <button type="button" disabled={granting} onClick={() => void grant()} className="inline-flex items-center justify-center gap-1.5 self-start rounded-lg bg-accent px-3 py-1.5 text-[12px] font-semibold text-on-accent disabled:opacity-40">
          <Icon name={granting ? "activity" : "link"} size={13} /> {granting ? "Waiting for approval…" : "Grant permissions"}
        </button>
      ) : null}
    </section>
  );
}

export default ComputerUsePanel;
