import { useCallback, useEffect, useState, type ReactNode } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import type { RuntimeTab } from "./runtimePaneTypes";
import {
  controlWorkflowRun,
  decideWorkflowApproval,
  listWorkflowInbox,
  listWorkflows,
  listWorkflowRuns,
  listWorkflowTriggers,
  listWorkflowVersions,
  startWorkflowRun,
  type WorkflowApproval,
  type WorkflowDefinition,
  type WorkflowRun,
  type WorkflowTrigger,
  type WorkflowVersion,
} from "@rightkit/agent-ui-core/lib/daemon/workflows";

function State({ children }: { children: ReactNode }) {
  return (
    <div className="p-6 text-[13px] text-text-2" role="status">
      {children}
    </div>
  );
}
function ErrorState({ message, retry }: { message: string; retry: () => void }) {
  return (
    <div className="p-6">
      <p role="alert">{message}</p>
      <button type="button" onClick={retry}>
        Retry
      </button>
    </div>
  );
}
export function inboxKindLabel(kind: WorkflowApproval["kind"]) {
  return (
    (
      {
        approval: "Approval required",
        input: "Input required",
        failure: "Workflow failure",
        uncertain_effect: "Effect outcome uncertain",
        deliverable: "Deliverable ready",
      } as Record<string, string>
    )[kind] ?? "Workflow attention"
  );
}
export type AutomationDetail = {
  version: string;
  trigger: string;
  nextRun: string;
  workspace: string;
  location: string;
  capabilities: string;
  budgets: string;
};
export function summarizeWorkflowVersion(
  version?: WorkflowVersion,
  trigger?: WorkflowTrigger,
): AutomationDetail {
  const source = version?.definitionJson
    ? (JSON.parse(version.definitionJson) as {
        definition?: { owner_scope?: string };
        nodes?: Array<{
          capabilities?: string[];
          retry?: { max_attempts?: number };
          config?: Record<string, unknown>;
        }>;
      })
    : {};
  const owner = source.definition?.owner_scope || "local";
  const capabilities = [
    ...new Set((source.nodes ?? []).flatMap((node) => node.capabilities ?? [])),
  ];
  const attempts = Math.max(
    0,
    ...(source.nodes ?? []).map((node) => Number(node.retry?.max_attempts ?? 0)),
  );
  const bounds = (source.nodes ?? []).flatMap((node) =>
    Object.entries(node.config ?? {})
      .filter(([key]) => key.startsWith("max_"))
      .map(([key, value]) => `${key}=${String(value)}`),
  );
  return {
    version: `v${version?.versionNumber ?? 0} · ${version?.publicationState ?? "draft"}`,
    trigger: trigger ? `${trigger.kind}${trigger.enabled ? "" : " · paused"}` : "manual",
    nextRun: trigger?.next_fire_at_unix_ms
      ? new Date(trigger.next_fire_at_unix_ms).toLocaleString()
      : "Manual",
    workspace: owner,
    location: owner.includes(":") ? owner.split(":").slice(1).join(":") : owner,
    capabilities: capabilities.join(", ") || "None",
    budgets: [`attempts=${attempts || 1}`, ...bounds].join(" · "),
  };
}

export function AutomationsPanel({ config }: { config: DaemonConfig | null }) {
  const [definitions, setDefinitions] = useState<WorkflowDefinition[]>([]);
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [details, setDetails] = useState<Record<string, AutomationDetail>>({});
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState("");
  const load = useCallback(async () => {
    if (!config) {
      setLoading(false);
      return;
    }
    setLoading(true);
    setError("");
    try {
      const [workflows, active] = await Promise.all([
        listWorkflows(config),
        listWorkflowRuns(config),
      ]);
      setDefinitions(workflows.items);
      setRuns(active.items);
      const rows = await Promise.all(
        workflows.items.map(async (workflow) => {
          const [versions, triggers] = await Promise.all([
            listWorkflowVersions(config, workflow.id),
            listWorkflowTriggers(config, workflow.id),
          ]);
          return [workflow.id, summarizeWorkflowVersion(versions[0], triggers.items[0])] as const;
        }),
      );
      setDetails(Object.fromEntries(rows));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not load automations");
    } finally {
      setLoading(false);
    }
  }, [config]);
  useEffect(() => {
    void load();
  }, [load]);
  async function runNow(workflow: WorkflowDefinition) {
    if (!config) return;
    setPending(workflow.id);
    try {
      await startWorkflowRun(config, workflow.id);
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not start workflow");
    } finally {
      setPending("");
    }
  }
  async function control(run: WorkflowRun, action: "cancel" | "pause" | "resume") {
    if (!config) return;
    setPending(run.id);
    try {
      await controlWorkflowRun(config, run.id, action);
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Workflow control failed");
    } finally {
      setPending("");
    }
  }
  if (!config) return <State>Connect to local daemon to manage automations.</State>;
  if (loading) return <State>Loading automations…</State>;
  if (error) return <ErrorState message={error} retry={() => void load()} />;
  return (
    <section aria-label="Automations" className="flex-1 overflow-y-auto p-6">
      <h3>Automations</h3>
      {definitions.length === 0 ? (
        <State>No automations yet.</State>
      ) : (
        definitions.map((workflow) => {
          const detail = details[workflow.id];
          return (
            <article data-workflow-definition key={workflow.id}>
              <h4>{workflow.name}</h4>
              {detail ? (
                <dl>
                  <dt>Version</dt>
                  <dd>{detail.version}</dd>
                  <dt>Trigger</dt>
                  <dd>{detail.trigger}</dd>
                  <dt>Next run</dt>
                  <dd>{detail.nextRun}</dd>
                  <dt>Workspace</dt>
                  <dd>{detail.workspace}</dd>
                  <dt>Location</dt>
                  <dd>{detail.location}</dd>
                  <dt>Capabilities</dt>
                  <dd>{detail.capabilities}</dd>
                  <dt>Budgets</dt>
                  <dd>{detail.budgets}</dd>
                </dl>
              ) : (
                <State>Loading details…</State>
              )}
              <button
                type="button"
                disabled={pending === workflow.id}
                onClick={() => void runNow(workflow)}
              >
                Run now
              </button>
            </article>
          );
        })
      )}
      <h4>Current & history</h4>
      {runs.length === 0 ? (
        <State>No workflow runs yet.</State>
      ) : (
        runs.map((run) => (
          <article data-workflow-run data-run-id={run.id} tabIndex={-1} key={run.id}>
            <p>
              {run.id} · {run.lifecyclePhase} · revision {run.lifecycleRevision}
            </p>
            <button type="button" onClick={() => focusWorkflowRun(run.id)}>
              Open history, artifacts & terminal
            </button>
            {run.lifecyclePhase === "running" && (
              <button type="button" onClick={() => void control(run, "pause")}>
                Pause
              </button>
            )}
            {run.lifecyclePhase === "suspended" && (
              <button type="button" onClick={() => void control(run, "resume")}>
                Resume
              </button>
            )}
            <button type="button" onClick={() => void control(run, "cancel")}>
              Cancel
            </button>
          </article>
        ))
      )}
    </section>
  );
}

/** Focus a run row in the Automations tab (it carries `data-run-id` and `tabIndex={-1}`). */
function focusWorkflowRun(runId: string) {
  requestAnimationFrame(() =>
    document.querySelector<HTMLElement>(`[data-run-id="${CSS.escape(runId)}"]`)?.focus(),
  );
}

export function WorkflowInboxPanel({
  config,
  onRuntimeTab,
}: {
  config: DaemonConfig | null;
  onRuntimeTab?: (tab: RuntimeTab) => void;
}) {
  // An item tied to a run opens that run in Automations; anything else stays in the inbox.
  const openInboxItem = (item: { runId?: string | null }) => {
    if (!item.runId) return;
    onRuntimeTab?.("automations");
    focusWorkflowRun(item.runId);
  };
  const [items, setItems] = useState<WorkflowApproval[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState("");
  const load = useCallback(async () => {
    if (!config) {
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      setItems((await listWorkflowInbox(config)).items);
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not load review inbox");
    } finally {
      setLoading(false);
    }
  }, [config]);
  useEffect(() => {
    void load();
  }, [load]);
  async function decide(item: WorkflowApproval, decision: "approved" | "denied") {
    if (!config) return;
    setPending(item.id);
    try {
      await decideWorkflowApproval(config, item.id, decision);
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Decision failed");
    } finally {
      setPending("");
    }
  }
  if (!config) return <State>Connect to local daemon to review workflow attention.</State>;
  if (loading) return <State>Loading review inbox…</State>;
  if (error) return <ErrorState message={error} retry={() => void load()} />;
  return (
    <section aria-label="Review Inbox" className="flex-1 overflow-y-auto p-6">
      <h3>Review Inbox</h3>
      {items.length === 0 ? (
        <State>Inbox clear.</State>
      ) : (
        items.map((item) => (
          <article data-workflow-inbox-item key={item.id}>
            <p>{inboxKindLabel(item.kind)}</p>
            <p>{item.title ?? item.summary ?? item.runId}</p>
            <button type="button" onClick={() => openInboxItem(item)}>
              Open details
            </button>
            {item.kind === "approval" ? (
              <>
                <button
                  type="button"
                  disabled={pending === item.id}
                  onClick={() => void decide(item, "approved")}
                >
                  Approve
                </button>
                <button
                  type="button"
                  disabled={pending === item.id}
                  onClick={() => void decide(item, "denied")}
                >
                  Deny
                </button>
              </>
            ) : (
              <button type="button" onClick={() => openInboxItem(item)}>
                {item.kind === "deliverable" ? "Acknowledge" : "Review"}
              </button>
            )}
          </article>
        ))
      )}
    </section>
  );
}
