import { useCallback, useEffect, useRef, useState } from "react";
import type {
  DaemonConfig,
  DriverBindingStatus,
  DriverDiscovery,
  DriverLease,
  DriverSessionStatus,
  ExternalDriverKind,
  SessionRow,
} from "@rightkit/agent-ui-core/lib/types";
import {
  discoverDriver,
  getDriverStatus,
  interruptDriver,
  issueDriverLease,
  resumeDriver,
  startDriver,
  stopDriver,
} from "@rightkit/agent-ui-core/lib/daemon";
import { CodeBlock } from "@rightkit/doc-render";
import { hostAppName } from "../../../appName";

const DRIVER_LABELS: Record<string, string> = {
  codex: "Codex",
  claude: "Claude",
};

const REPAIR_COMMANDS: Record<string, string> = {
  codex: "codex login",
  claude: "claude login",
};

const EXTERNAL_DRIVER_KINDS: ExternalDriverKind[] = ["codex", "claude"];

export interface ConductorsTabProps {
  config: DaemonConfig | null;
  selectedSession: SessionRow | null;
}

export default function ConductorsTab({ config, selectedSession }: ConductorsTabProps) {
  return (
    <section className="flex max-w-[720px] flex-col gap-4 p-6">
      <div className="min-w-0">
        <h3 className="text-[16px] font-bold text-text">Conductors</h3>
        <p className="text-[12px] text-text-3">
          {hostAppName()} launches your own installed Codex or Claude Code CLI (for example{" "}
          <code>codex app-server --stdio --strict-config</code>), gives it exactly one MCP server —
          {hostAppName()}&apos;s session-scoped capability gateway — and disables its native mutation
          routes, so every shell command and file write it makes is governed. Same login, same
          subscription.
        </p>
      </div>

      {EXTERNAL_DRIVER_KINDS.map((kind) => (
        <ConductorCard key={kind} kind={kind} config={config} />
      ))}

      <SessionDriverControls config={config} session={selectedSession} />
    </section>
  );
}

function ConductorCard({
  kind,
  config,
}: {
  kind: ExternalDriverKind;
  config: DaemonConfig | null;
}) {
  const [discovery, setDiscovery] = useState<DriverDiscovery | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  const refresh = useCallback(async () => {
    if (!config) return;
    setPending(true);
    setError(null);
    try {
      const next = await discoverDriver(config, kind);
      setDiscovery(next);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : `${DRIVER_LABELS[kind]} discovery failed`);
    } finally {
      setPending(false);
    }
  }, [config, kind]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const diagnostic = error ?? discovery?.diagnostic ?? null;
  const tools = discovery?.capabilities.tools ?? [];

  return (
    <section
      className="grid gap-3 rounded-lg border border-line bg-panel p-4"
      aria-label={`${DRIVER_LABELS[kind]} conductor`}
    >
      <div className="flex items-center justify-between gap-3">
        <h4 className="text-[13px] font-semibold text-text">{DRIVER_LABELS[kind]}</h4>
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={pending || !config}
          onClick={() => void refresh()}
        >
          {pending ? "Checking…" : "Refresh"}
        </button>
      </div>
      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-[12px]">
        <dt className="text-text-3">Installed</dt>
        <dd>{discovery ? (discovery.installed ? "Yes" : "No") : "Checking…"}</dd>
        <dt className="text-text-3">Authenticated</dt>
        <dd>{discovery ? (discovery.authRequired ? "Needs login" : "Yes") : "Checking…"}</dd>
        <dt className="text-text-3">Binary version</dt>
        <dd>{discovery?.version.binaryVersion ?? "—"}</dd>
        <dt className="text-text-3">Protocol version</dt>
        <dd>{discovery?.version.protocolVersion ?? "—"}</dd>
        <dt className="text-text-3">Governed tools</dt>
        <dd className="truncate">
          {tools.length > 0 ? `${tools.length} (${tools.map((tool) => tool.id).join(", ")})` : "—"}
        </dd>
      </dl>
      {diagnostic ? (
        <div className="grid gap-2">
          <p className="rounded border border-red-500/30 bg-red-500/10 p-2 text-[12px] text-red-300">
            {diagnostic}
          </p>
          <CodeBlock code={REPAIR_COMMANDS[kind]} language="shell" />
        </div>
      ) : null}
    </section>
  );
}

function SessionDriverControls({
  config,
  session,
}: {
  config: DaemonConfig | null;
  session: SessionRow | null;
}) {
  const kind = session?.driverKind;
  const externalKind = kind === "codex" || kind === "claude" ? kind : null;
  const [discovery, setDiscovery] = useState<DriverDiscovery | null>(null);
  const [status, setStatus] = useState<DriverSessionStatus | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const leaseRef = useRef<DriverLease | null>(null);

  const refresh = useCallback(async () => {
    if (!config || !session || !externalKind) return;
    setError(null);
    try {
      const [nextDiscovery, nextStatus] = await Promise.all([
        discoverDriver(config, externalKind),
        getDriverStatus(config, session.id),
      ]);
      setDiscovery(nextDiscovery);
      setStatus(nextStatus);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Driver status could not be loaded");
    }
  }, [config, externalKind, session]);

  useEffect(() => {
    leaseRef.current = null;
    setDiscovery(null);
    setStatus(null);
    void refresh();
  }, [refresh]);

  if (!externalKind) {
    return (
      <section className="grid gap-2 rounded-lg border border-line bg-panel p-4">
        <h4 className="text-[13px] font-semibold text-text">Active session</h4>
        <p className="text-[12px] text-text-3">
          Select a session that runs on Codex or Claude to start, resume, interrupt, or stop its
          driver here.
        </p>
      </section>
    );
  }
  const driverKind: ExternalDriverKind = externalKind;
  const binding = status?.drivers.find((item) => item.driverKind === externalKind) ?? null;
  const diagnostic = error ?? binding?.diagnostic ?? discovery?.diagnostic ?? null;
  const repairCommand = REPAIR_COMMANDS[externalKind];

  async function withAction(name: string, action: () => Promise<void>) {
    setPending(name);
    setError(null);
    try {
      await action();
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : `Driver ${name} failed`);
    } finally {
      setPending(null);
    }
  }

  async function leaseFor(bindingForLease?: DriverBindingStatus | null) {
    if (!config || !session || !discovery?.version.binaryVersion || !discovery.version.schemaHash) {
      throw new Error("Driver discovery must report version and schema before it can start");
    }
    const lease = await issueDriverLease(config, session.id, {
      driverKind,
      binaryVersion: bindingForLease?.version.binaryVersion ?? discovery.version.binaryVersion,
      schemaHash: bindingForLease?.version.schemaHash ?? discovery.version.schemaHash,
    });
    leaseRef.current = lease;
    return lease;
  }

  return (
    <section
      className="grid gap-3 rounded-lg border border-line bg-panel p-4"
      aria-label="Driver session"
    >
      <div>
        <h4 className="text-[13px] font-semibold text-text">
          {DRIVER_LABELS[externalKind]} driver — {session?.title ?? session?.id}
        </h4>
        <p className="text-[12px] text-text-3">
          Session-scoped gateway only; native vendor effects remain disabled.
        </p>
      </div>
      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-[12px]">
        <dt className="text-text-3">Lifecycle</dt>
        <dd>{binding?.lifecycle ?? "not started"}</dd>
        <dt className="text-text-3">Gateway</dt>
        <dd>{binding?.gatewayState.state ?? "not started"}</dd>
        <dt className="text-text-3">Vendor session id</dt>
        <dd className="truncate">{binding?.vendorSessionId ?? "—"}</dd>
        <dt className="text-text-3">Generation</dt>
        <dd>{binding?.processGeneration ?? "—"}</dd>
      </dl>
      {diagnostic ? (
        <div className="grid gap-2">
          <p className="rounded border border-red-500/30 bg-red-500/10 p-2 text-[12px] text-red-300">
            {diagnostic}
          </p>
          <CodeBlock code={repairCommand} language="shell" />
        </div>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={pending !== null}
          onClick={() => void withAction("refresh", refresh)}
        >
          {pending === "refresh" ? "Refreshing…" : "Refresh binary & auth"}
        </button>
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={pending !== null || !discovery?.installed || discovery.authRequired}
          onClick={() =>
            void withAction("start", async () => {
              if (!config || !session) return;
              const lease = await leaseFor();
              await startDriver(
                config,
                session.id,
                lease.capabilityToken,
                lease.bindingId,
                discovery?.version.executable,
              );
            })
          }
        >
          {pending === "start" ? "Starting…" : "Start"}
        </button>
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={pending !== null || !binding?.vendorSessionId || !leaseRef.current}
          onClick={() =>
            void withAction("resume", async () => {
              if (!config || !session || !binding?.vendorSessionId) return;
              const lease = leaseRef.current ?? (await leaseFor(binding));
              await resumeDriver(
                config,
                session.id,
                lease.capabilityToken,
                binding.bindingId,
                binding.vendorSessionId,
              );
            })
          }
        >
          Resume
        </button>
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={
            pending !== null ||
            !binding?.vendorSessionId ||
            !binding.vendorTurnId ||
            !leaseRef.current
          }
          onClick={() =>
            void withAction("interrupt", async () => {
              if (
                !config ||
                !session ||
                !binding?.vendorSessionId ||
                !binding.vendorTurnId ||
                !leaseRef.current
              )
                return;
              await interruptDriver(
                config,
                session.id,
                leaseRef.current.capabilityToken,
                binding.bindingId,
                binding.vendorSessionId,
                binding.vendorTurnId,
              );
            })
          }
        >
          Interrupt
        </button>
        <button
          type="button"
          className="cr-control-pill px-3 py-1 text-[12px]"
          disabled={pending !== null || !binding || !leaseRef.current}
          onClick={() =>
            void withAction("stop", async () => {
              if (!config || !session || !binding || !leaseRef.current) return;
              await stopDriver(
                config,
                session.id,
                leaseRef.current.capabilityToken,
                binding.bindingId,
              );
              leaseRef.current = null;
            })
          }
        >
          Stop
        </button>
      </div>
    </section>
  );
}
