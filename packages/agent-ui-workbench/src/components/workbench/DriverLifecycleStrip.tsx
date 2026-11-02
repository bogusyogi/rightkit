import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { getDriverStatus } from "@rightkit/agent-ui-core/lib/daemon";
import {
  describeDriverRecovery,
  describeDriverStatusError,
  didResumeFromSnapshot,
  isReconnectingBinding,
  type DriverPollSnapshot,
  type DriverRecoveryReason,
} from "@rightkit/agent-ui-core/lib/daemon/drivers";
import type {
  DaemonConfig,
  DriverBindingStatus,
  DriverLifecycle,
  ExternalDriverKind,
} from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../appName.js";

const POLL_MS = 2_000;

type Tone = "ready" | "active" | "attention" | "failed" | "idle";

const lifecyclePresentation: Record<
  DriverLifecycle | "waiting_for_child" | "not_started" | "status_unavailable",
  { label: string; tone: Tone; detail: string }
> = {
  unavailable: {
    label: "Unavailable",
    tone: "failed",
    detail: "Driver is not installed or could not be discovered.",
  },
  authentication_required: {
    label: "Login required",
    tone: "attention",
    detail: "Authenticate this driver before starting work.",
  },
  leased: {
    label: "Starting",
    tone: "active",
    detail: "Governed capabilities are ready for this session.",
  },
  starting: {
    label: "Starting",
    tone: "active",
    detail: "Driver process is connecting.",
  },
  ready: {
    label: "Ready",
    tone: "ready",
    detail: "Driver is connected and ready.",
  },
  running: {
    label: "Active",
    tone: "active",
    detail: `Driver is working through ${hostAppName()}.`,
  },
  waiting_for_approval: {
    label: "Waiting for approval",
    tone: "attention",
    detail: "Review the pending governed action in this session.",
  },
  waiting_for_child: {
    label: "Waiting for child",
    tone: "active",
    detail: "Driver is waiting for a delegated agent.",
  },
  interrupted: {
    label: "Interrupted",
    tone: "attention",
    detail: "Driver stopped mid-turn and can be resumed.",
  },
  completed: {
    label: "Completed",
    tone: "ready",
    detail: "Driver finished this turn.",
  },
  recovery_required: {
    label: "Recovery required",
    tone: "attention",
    detail: "Review the durable claim, then restart or resume this driver.",
  },
  failed: {
    label: "Failed",
    tone: "failed",
    detail: "Driver needs attention before work can continue.",
  },
  stopped: {
    label: "Stopped",
    tone: "idle",
    detail: "Driver is not running.",
  },
  not_started: {
    label: "Ready to start",
    tone: "idle",
    detail: "Send a message and this session's driver starts automatically.",
  },
  status_unavailable: {
    label: "Status unavailable",
    tone: "failed",
    detail: "Driver status could not be loaded.",
  },
};

function statusToneClass(tone: Tone) {
  switch (tone) {
    case "ready":
      return "bg-success";
    case "active":
      return "bg-accent";
    case "attention":
      return "bg-warning";
    case "failed":
      return "bg-danger";
    default:
      return "bg-text-3/50";
  }
}

const recoveryPresentation: Record<
  DriverRecoveryReason,
  { label: string; tone: Tone; actionLabel: string }
> = {
  lost_process: {
    label: "Driver lost",
    tone: "attention",
    actionLabel: "Restart driver",
  },
  unsupported_schema: {
    label: "Unsupported driver version",
    tone: "attention",
    actionLabel: "Open conductor settings",
  },
  lease_expired: {
    label: "Driver access expired",
    tone: "attention",
    actionLabel: "Reconnect",
  },
};

function recoveryDetail(
  reason: DriverRecoveryReason,
  binding: DriverBindingStatus | null,
  error: string | null,
) {
  switch (reason) {
    case "lost_process":
      return (
        binding?.diagnostic ??
        "A prior driver process stopped after claiming work. Review the durable claim before restarting."
      );
    case "unsupported_schema":
      return `This driver reports protocol "${binding?.version.protocolVersion ?? "unknown"}", which ${hostAppName()} doesn't support yet.`;
    case "lease_expired":
      return error ?? "This session's driver access expired.";
  }
}

function activeBinding(drivers: DriverBindingStatus[], driverKind: ExternalDriverKind) {
  return drivers.find((driver) => driver.driverKind === driverKind) ?? null;
}

export default function DriverLifecycleStrip({
  config,
  sessionId,
  driverKind,
  onManage,
}: {
  config: DaemonConfig | null;
  sessionId: string;
  driverKind: ExternalDriverKind;
  onManage: () => void;
}) {
  const [binding, setBinding] = useState<DriverBindingStatus | null>(null);
  const [aggregateLifecycle, setAggregateLifecycle] = useState<
    "ready" | "authentication_required" | "not_resumed" | null
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [errorStatus, setErrorStatus] = useState<number | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [justResumed, setJustResumed] = useState(false);
  const previousSnapshotRef = useRef<DriverPollSnapshot | null>(null);

  const refresh = useCallback(async () => {
    if (!config) {
      setError("Daemon not connected.");
      setErrorStatus(null);
      return;
    }
    setRefreshing(true);
    try {
      const next = await getDriverStatus(config, sessionId);
      const nextBinding = activeBinding(next.drivers, driverKind);
      const snapshot: DriverPollSnapshot | null = nextBinding
        ? { lifecycle: nextBinding.lifecycle, vendorSessionId: nextBinding.vendorSessionId }
        : null;
      setJustResumed(didResumeFromSnapshot(previousSnapshotRef.current, snapshot));
      previousSnapshotRef.current = snapshot;
      setBinding(nextBinding);
      setAggregateLifecycle(
        next.lifecycle === "authentication_required" || next.lifecycle === "not_resumed"
          ? next.lifecycle
          : "ready",
      );
      setError(null);
      setErrorStatus(null);
    } catch (cause) {
      // Never surface the raw wire-protocol error (operationId + HTTP
      // status) here — describeDriverStatusError translates it into a calm,
      // human message and passes through any genuine diagnostic untouched.
      const described = describeDriverStatusError(cause);
      setError(described.message);
      setErrorStatus(described.status);
    } finally {
      setRefreshing(false);
    }
  }, [config, driverKind, sessionId]);

  useEffect(() => {
    setBinding(null);
    setAggregateLifecycle(null);
    setError(null);
    setErrorStatus(null);
    setJustResumed(false);
    previousSnapshotRef.current = null;
    void refresh();
    const timer = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const recoveryReason = describeDriverRecovery(binding, error ? errorStatus : null);
  const reconnecting = !recoveryReason && !error && isReconnectingBinding(binding);
  const resumed = !recoveryReason && !error && !reconnecting && justResumed;

  const lifecycle = error
    ? "status_unavailable"
    : (binding?.lifecycle ??
      // A binding always reports its own lifecycle, so the daemon's
      // session-level "not_resumed" could only ever surface when there was no
      // binding — i.e. nothing to resume. Telling the user to "reconnect the
      // driver" in that state pointed at a control that could not help them.
      (aggregateLifecycle === "authentication_required"
        ? "authentication_required"
        : "not_started"));

  let label: string;
  let tone: Tone;
  let detail: string;
  let primaryActionLabel = "Manage";
  let primaryAction = onManage;
  const showRetry = Boolean(error) && !recoveryReason;

  if (recoveryReason) {
    const info = recoveryPresentation[recoveryReason];
    label = info.label;
    tone = info.tone;
    detail = recoveryDetail(recoveryReason, binding, error);
    primaryActionLabel = info.actionLabel;
    primaryAction = recoveryReason === "lease_expired" ? () => void refresh() : onManage;
  } else if (reconnecting) {
    label = "Reconnecting";
    tone = "attention";
    detail = "Re-establishing the existing driver session.";
  } else if (resumed) {
    label = "Resumed";
    tone = "ready";
    detail = "The driver picked back up from where it left off.";
  } else {
    const presentation = lifecyclePresentation[lifecycle];
    const diagnostic = error ?? binding?.diagnostic ?? null;
    const gatewayFailure =
      binding?.gatewayState.state === "failed" ? binding.gatewayState.message : null;
    label = presentation.label;
    tone = presentation.tone;
    detail = gatewayFailure ?? diagnostic ?? presentation.detail;
  }

  const driverLabel =
    (
      {
        claude: "Claude",
        codex: "Codex",
        grok: "Grok Build",
        cursor: "Cursor Agent",
        cline: "Cline",
        commandcode: "Command Code",
      } as Record<string, string>
    )[driverKind] ?? driverKind;
  const version = binding?.version.binaryVersion;
  const metadata = useMemo(
    () =>
      [version, binding?.gatewayState.state === "ready" ? "gateway connected" : null]
        .filter(Boolean)
        .join(" · "),
    [binding?.gatewayState.state, version],
  );

  return (
    <section
      data-testid="driver-lifecycle-strip"
      data-driver-kind={driverKind}
      data-driver-lifecycle={lifecycle}
      data-driver-recovery={recoveryReason ?? undefined}
      className="mb-1 flex items-center gap-2 rounded-lg border border-line bg-panel px-3 py-2"
      aria-label={`${driverLabel} conductor status`}
    >
      <span
        className={`h-2 w-2 shrink-0 rounded-full ${statusToneClass(tone)}`}
        aria-hidden="true"
      />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2 text-[12px]">
          <span className="font-semibold text-text">{driverLabel}</span>
          <span className="text-text-2">{label}</span>
          {metadata ? <span className="truncate text-[10px] text-text-3">{metadata}</span> : null}
        </div>
        <p className="truncate text-[10px] leading-4 text-text-3" title={detail}>
          {detail}
        </p>
      </div>
      {recoveryReason ? (
        <button
          type="button"
          className="cr-control-pill shrink-0 px-2 py-1 text-[11px]"
          disabled={recoveryReason === "lease_expired" && refreshing}
          onClick={primaryAction}
        >
          {recoveryReason === "lease_expired" && refreshing ? "Reconnecting…" : primaryActionLabel}
        </button>
      ) : (
        <>
          {showRetry ? (
            <button
              type="button"
              className="cr-control-pill shrink-0 px-2 py-1 text-[11px]"
              disabled={refreshing}
              onClick={() => void refresh()}
            >
              {refreshing ? "Retrying…" : "Retry"}
            </button>
          ) : null}
          <button
            type="button"
            className="cr-control-pill shrink-0 px-2 py-1 text-[11px]"
            onClick={onManage}
          >
            Manage
          </button>
        </>
      )}
    </section>
  );
}
