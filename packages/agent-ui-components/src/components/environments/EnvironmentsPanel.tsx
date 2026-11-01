// D4 — EnvironmentsPanel: Local / Worktree / SSH / WSL / configured remote
// selection with root / branch / trust / setup / latency / provider state and
// test-connection. Setup scripts are versioned, logged, cancellable, and
// rerunnable.
//
// Component contract:
//   - props: workspaceRoot + config
//   - state from `useEnvironmentsStore` (list + testResults + scripts + runs)
//   - honest "configured but not runtime-supported" surface for SSH / WSL /
//     configuredRemote (no fake test results)
//   - setup scripts always expose their version + steps + log tail
//
// This component owns only D4 surface area. It does NOT edit AppBody,
// WorkbenchContent, TitleBar, or Settings.

import { useEffect, useState } from "react";

import { useEnvironmentsStore } from "@rightkit/agent-ui-core/state/environmentsStore";
import { environmentKindLabel } from "@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention";
import type { DaemonConfig, EnvironmentRecord } from "@rightkit/agent-ui-core/lib/types";
import { hostAppName } from "../../appName.js";

interface EnvironmentsPanelProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
}

function trustBadge(env: EnvironmentRecord): string {
  switch (env.trust) {
    case "trusted":
      return "trusted";
    case "sandboxed":
      return "sandboxed";
    case "locked":
      return "locked";
    case "untrusted":
      return "untrusted";
  }
}

function setupBadge(env: EnvironmentRecord): string {
  switch (env.setupState) {
    case "ready":
      return "ready";
    case "pending":
      return "pending";
    case "running":
      return "running";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
    case "unsupported":
      return "unsupported";
  }
}

export default function EnvironmentsPanel({ config, workspaceRoot }: EnvironmentsPanelProps) {
  const list = useEnvironmentsStore((state) => state.list);
  const listPending = useEnvironmentsStore((state) => state.listPending);
  const selectedId = useEnvironmentsStore((state) => state.selectedId);
  const fallbackId = useEnvironmentsStore((state) => state.fallbackId);
  const testResults = useEnvironmentsStore((state) => state.testResults);
  const scripts = useEnvironmentsStore((state) => state.scripts);
  const scriptsPending = useEnvironmentsStore((state) => state.scriptsPending);
  const scriptRuns = useEnvironmentsStore((state) => state.scriptRuns);
  const actionPending = useEnvironmentsStore((state) => state.actionPending);
  const error = useEnvironmentsStore((state) => state.error);
  const reset = useEnvironmentsStore((state) => state.reset);
  const loadList = useEnvironmentsStore((state) => state.loadList);
  const select = useEnvironmentsStore((state) => state.select);
  const test = useEnvironmentsStore((state) => state.test);
  const loadScripts = useEnvironmentsStore((state) => state.loadScripts);
  const runScript = useEnvironmentsStore((state) => state.runScript);
  const cancelRun = useEnvironmentsStore((state) => state.cancelRun);
  const rerunRun = useEnvironmentsStore((state) => state.rerunRun);
  const loadRunLogs = useEnvironmentsStore((state) => state.loadRunLogs);

  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const environments = list?.environments ?? [];
  const selectedEnvironment = environments.find((environment) => environment.id === selectedId);
  const fallbackEnvironment = environments.find((environment) => environment.id === fallbackId);
  const fallbackRequired = Boolean(
    fallbackEnvironment && !selectedEnvironment?.runtimeSupported,
  );

  useEffect(() => {
    reset();
    if (!config || !workspaceRoot.trim()) return;
    void loadList(config, workspaceRoot);
    void loadScripts(config, workspaceRoot);
  }, [config, workspaceRoot, reset, loadList, loadScripts]);

  return (
    <section
      aria-label="Environments"
      className="grid gap-3 rounded-xl border border-line bg-panel p-3"
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold text-text">Environment</h2>
          <p className="mt-0.5 text-[12px] text-text-3">
            {selectedEnvironment?.runtimeSupported
              ? `${selectedEnvironment.label} is the active workspace runtime.`
              : fallbackRequired
                ? "The saved runtime is unavailable; choose the visible fallback below."
                : "Choose the runtime this workspace should use."}
          </p>
        </div>
        <span className="rounded-full border border-line px-2 py-1 text-[11px] text-text-3">
          {listPending
            ? "Checking"
            : selectedEnvironment?.runtimeSupported
              ? "Ready"
              : fallbackRequired
                ? "Fallback needed"
                : "Not selected"}
        </span>
      </header>

      {fallbackRequired && fallbackEnvironment ? (
        <div
          className="grid gap-2 rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-2"
          data-testid="environment-fallback"
          role="status"
        >
          <p>
            {selectedEnvironment
              ? `${selectedEnvironment.label} is unavailable.`
              : "No saved environment is available."} Select {fallbackEnvironment.label} to
            route this workspace explicitly; {hostAppName()} will not switch silently.
          </p>
          <button
            type="button"
            disabled={!config || actionPending === `select:${fallbackEnvironment.id}`}
            onClick={() =>
              config && void select(config, workspaceRoot, fallbackEnvironment.id)
            }
            className="h-8 w-fit rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
          >
            {actionPending === `select:${fallbackEnvironment.id}`
              ? "Selecting…"
              : `Use ${fallbackEnvironment.label} fallback`}
          </button>
        </div>
      ) : null}

      <ul className="grid gap-2" data-testid="environment-list">
        {environments.map((env) => {
          const testResult = testResults[env.id];
          return (
            <li
              key={env.id}
              className={
                selectedId === env.id && env.runtimeSupported
                  ? "grid gap-2 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_8%,var(--color-bg))] p-3 text-[12px]"
                  : "grid gap-2 rounded-lg border border-line bg-bg p-3 text-[12px]"
              }
              data-testid={`environment-${env.id}`}
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-semibold text-text">{env.label}</span>
                <span className="text-text-3">{environmentKindLabel(env.kind)}</span>
                <span
                  className={env.runtimeSupported ? "text-accent-2" : "text-text-3"}
                  data-testid={`runtime-supported-${env.id}`}
                >
                  {env.runtimeSupported ? "ready" : "unavailable"}
                </span>
                <span className="text-text-3">
                  {trustBadge(env)} · {setupBadge(env)}
                </span>
                {selectedId === env.id && env.runtimeSupported ? (
                  <span className="font-semibold text-accent">Active</span>
                ) : null}
              </div>

              {env.runtimeUnavailableReason ? (
                <p className="rounded-md bg-panel px-2 py-1 text-[12px] text-text-3">
                  {env.runtimeUnavailableReason}
                </p>
              ) : null}
              {env.setupError ? (
                <p className="rounded-md bg-panel px-2 py-1 text-[12px] text-text-3">
                  Setup failed: {env.setupError}
                </p>
              ) : null}
              <p className="text-[11px] text-text-3">
                {env.workspaceRoot}
                {env.branch ? ` · ${env.branch}` : ""}
                {env.lastLatencyMs !== null && env.lastLatencyMs !== undefined
                  ? ` · ${env.lastLatencyMs}ms`
                  : ""}
              </p>

              {env.providers.length > 0 ? (
                <ul className="flex flex-wrap gap-2">
                  {env.providers.map((provider) => (
                    <li
                      key={provider.provider}
                      className="rounded-md border border-line bg-panel px-2 py-1 text-[11px] text-text-2"
                    >
                      {provider.provider}
                      {provider.defaultModel ? ` · ${provider.defaultModel}` : ""}
                      {provider.available ? "" : " · unavailable"}
                    </li>
                  ))}
                </ul>
              ) : null}

              <div className="flex flex-wrap items-center gap-2">
                <button
                  type="button"
                  disabled={
                    !config || !env.runtimeSupported || actionPending === `select:${env.id}`
                  }
                  onClick={() => config && void select(config, workspaceRoot, env.id)}
                  className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
                >
                  {selectedId === env.id && env.runtimeSupported
                    ? "Active"
                    : actionPending === `select:${env.id}`
                      ? "Selecting…"
                      : "Use this environment"}
                </button>
                <button
                  type="button"
                  disabled={!config || !env.runtimeSupported || actionPending === `test:${env.id}`}
                  onClick={() => config && void test(config, workspaceRoot, env.id)}
                  className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
                >
                  {actionPending === `test:${env.id}` ? "Testing…" : "Test"}
                </button>
              </div>

              {testResult ? (
                <div
                  className="rounded-md bg-panel px-2 py-1 text-[11px] text-text-2"
                  data-testid={`test-result-${env.id}`}
                >
                  <code className="font-mono">{testResult.command}</code>
                  <span className="ml-2">
                    {testResult.reachable ? "reachable" : "unreachable"}
                    {testResult.latencyMs !== null && testResult.latencyMs !== undefined
                      ? ` · ${testResult.latencyMs}ms`
                      : ""}
                  </span>
                  {testResult.error ? (
                    <span className="ml-2 text-text-3">{testResult.error}</span>
                  ) : null}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>

      <details className="rounded-lg border border-line bg-bg" data-testid="environment-setup">
        <summary className="cursor-pointer px-3 py-2 text-[12px] font-semibold text-text-2">
          Setup &amp; diagnostics{" "}
          <span className="ml-1 font-normal text-text-3">
            {scripts.length ? `${scripts.length} script${scripts.length === 1 ? "" : "s"}` : ""}
          </span>
        </summary>
        <div className="grid gap-3 border-t border-line p-3">
          <h3 className="text-[12px] font-semibold text-text-2">Setup scripts</h3>
          {scriptsPending ? <p className="text-[12px] text-text-3">loading scripts…</p> : null}
          <ul className="grid gap-2" data-testid="setup-scripts">
            {scripts.map((script) => (
              <li
                key={script.id}
                className="grid gap-2 rounded-lg border border-line bg-bg p-3 text-[12px]"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold text-text">{script.label}</span>
                  <span className="text-text-3">
                    v{script.version} · {script.id}
                  </span>
                  <span className="text-text-3">
                    {environmentKindLabel(script.environmentKind)}
                  </span>
                </div>
                <details>
                  <summary className="cursor-pointer text-[11px] text-text-3">
                    Show {script.steps.length} step{script.steps.length === 1 ? "" : "s"}
                  </summary>
                  <ol className="mt-2 list-decimal pl-6 text-text-3">
                    {script.steps.map((step) => (
                      <li key={step.id}>
                        <code className="font-mono">
                          {step.command} {step.args.join(" ")}
                        </code>{" "}
                        — {step.label}
                      </li>
                    ))}
                  </ol>
                </details>
                <div>
                  <button
                    type="button"
                    disabled={!config || actionPending === `run:${script.id}`}
                    onClick={() => config && void runScript(config, workspaceRoot, script.id)}
                    className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
                  >
                    Run
                  </button>
                </div>
              </li>
            ))}
          </ul>
          {Object.keys(scriptRuns).length > 0 ? (
            <div className="grid gap-2">
              <h3 className="text-[12px] font-semibold text-text-2">Setup runs</h3>
              <ul className="grid gap-2" data-testid="setup-runs">
                {Object.values(scriptRuns).map((run) => {
                  const expanded = expandedRunId === run.runId;
                  return (
                    <li
                      key={run.runId}
                      className="grid gap-2 rounded-lg border border-line bg-panel p-3 text-[12px]"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-semibold text-text">{run.scriptId}</span>
                        <span className="text-text-3">v{run.scriptVersion}</span>
                        <span
                          className={
                            run.state === "completed"
                              ? "text-accent-2"
                              : run.state === "failed" ||
                                  run.state === "timedOut" ||
                                  run.state === "unsupported"
                                ? "text-text-3"
                                : "text-text-2"
                          }
                        >
                          {run.state}
                          {run.exitCode !== null ? ` · exit ${run.exitCode}` : ""}
                        </span>
                      </div>
                      <div className="flex flex-wrap items-center gap-2">
                        <button
                          type="button"
                          onClick={() => setExpandedRunId(expanded ? null : run.runId)}
                          className="h-7 rounded-md border border-line bg-bg px-2 text-[11px] text-text-2 hover:text-text"
                        >
                          {expanded ? "Hide logs" : "Show logs"}
                        </button>
                        <button
                          type="button"
                          disabled={!config || actionPending === `logs:${run.runId}`}
                          onClick={() =>
                            config && void loadRunLogs(config, workspaceRoot, run.runId)
                          }
                          className="h-7 rounded-md border border-line bg-bg px-2 text-[11px] text-text-2 hover:text-text disabled:opacity-40"
                        >
                          {actionPending === `logs:${run.runId}` ? "Refreshing…" : "Refresh logs"}
                        </button>
                        {run.state === "running" || run.state === "pending" ? (
                          <button
                            type="button"
                            disabled={!config || actionPending === `cancel:${run.runId}`}
                            onClick={() =>
                              config &&
                              void cancelRun(config, workspaceRoot, run.runId, run.scriptId)
                            }
                            className="h-7 rounded-md border border-line bg-bg px-2 py-1 text-[11px] text-text-2 hover:text-text disabled:opacity-40"
                          >
                            {actionPending === `cancel:${run.runId}` ? "Cancelling…" : "Cancel"}
                          </button>
                        ) : (
                          <button
                            type="button"
                            disabled={!config || actionPending === `rerun:${run.runId}`}
                            onClick={() =>
                              config &&
                              void rerunRun(config, workspaceRoot, run.runId, run.scriptId)
                            }
                            className="h-7 rounded-md border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-2 py-1 text-[11px] font-semibold text-text disabled:opacity-40"
                          >
                            {actionPending === `rerun:${run.runId}` ? "Rerunning…" : "Rerun"}
                          </button>
                        )}
                      </div>
                      {expanded ? (
                        <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-bg px-2 py-1 font-mono text-[11px] text-text">
                          {run.logTail
                            .map((line) => `[${line.ts}] ${line.stream}: ${line.line}`)
                            .join("\n")}
                        </pre>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            </div>
          ) : null}
        </div>
      </details>

      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text"
        >
          {error}
        </p>
      ) : null}
    </section>
  );
}
