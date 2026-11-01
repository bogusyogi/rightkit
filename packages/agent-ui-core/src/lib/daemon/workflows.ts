import type { DaemonConfig } from "../types";
import { requestDaemon } from "./delivery-environments-attention";

export type WorkflowDefinition = {
  id: string;
  name: string;
  sourceKind: string;
  createdAt: string;
  updatedAt: string;
};
export type WorkflowRun = {
  id: string;
  workflowId: string;
  workflowVersionId: string;
  lifecyclePhase: string;
  lifecycleRevision: number;
  startedAt?: string;
  terminalAt?: string | null;
};
export type WorkflowVersion = {
  id: string;
  versionNumber: number;
  publicationState: string;
  definitionJson: string;
  compiledPlanJson: string;
  createdAt: string;
  notes?: string;
};
export type WorkflowTrigger = {
  id: string;
  kind: string;
  enabled: boolean;
  cron_expression?: string | null;
  next_fire_at_unix_ms?: number | null;
  concurrency_cap?: number;
};
export type WorkflowApproval = {
  id: string;
  kind: "approval" | "input" | "failure" | "uncertain_effect" | "deliverable" | string;
  state: string;
  title?: string;
  summary?: string;
  runId?: string | null;
  nodeId?: string | null;
  approvalId?: string | null;
  effectId?: string | null;
  deepLink?: unknown;
  role?: string;
  expiresAt?: string;
};
export type WorkflowEffect = {
  id: string;
  runId: string;
  nodeId: string;
  adapter: string;
  operation: string;
  state: string;
};
export type WorkflowList<T> = { items: T[]; nextCursor?: string | number | null };

export function listWorkflows(config: DaemonConfig) {
  return requestDaemon<WorkflowList<WorkflowDefinition>>(config, "/v1/workflows", {
    method: "GET",
  });
}
export function listWorkflowRuns(config: DaemonConfig) {
  return requestDaemon<WorkflowList<WorkflowRun>>(config, "/v1/runs", { method: "GET" });
}
export function listWorkflowVersions(config: DaemonConfig, workflowId: string) {
  return requestDaemon<WorkflowVersion[]>(config, "/v1/workflows/:id/versions", {
    method: "GET",
    pathParams: { id: workflowId },
  });
}
export function listWorkflowTriggers(config: DaemonConfig, workflowId: string) {
  return requestDaemon<WorkflowList<WorkflowTrigger>>(config, "/v1/workflows/:id/triggers", {
    method: "GET",
    pathParams: { id: workflowId },
  });
}
export function startWorkflowRun(config: DaemonConfig, workflowId: string, versionId?: string) {
  return requestDaemon<WorkflowRun>(config, "/v1/workflows/:id/runs", {
    method: "POST",
    pathParams: { id: workflowId },
    body: versionId ? { version_id: versionId } : {},
  });
}
export function listWorkflowInbox(config: DaemonConfig) {
  return requestDaemon<WorkflowList<WorkflowApproval>>(config, "/v1/workflow-inbox", {
    method: "GET",
  });
}
export function decideWorkflowApproval(
  config: DaemonConfig,
  attentionId: string,
  decision: "approved" | "denied",
) {
  return requestDaemon<WorkflowApproval>(config, "/v1/workflow-inbox/:id/decision", {
    method: "POST",
    pathParams: { id: attentionId },
    body: { decision },
  });
}
export function controlWorkflowRun(
  config: DaemonConfig,
  runId: string,
  action: "cancel" | "pause" | "resume",
) {
  return requestDaemon<WorkflowRun>(config, `/v1/runs/:id/${action}`, {
    method: "POST",
    pathParams: { id: runId },
  });
}
