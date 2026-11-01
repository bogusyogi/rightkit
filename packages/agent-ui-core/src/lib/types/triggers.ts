export type WebhookTriggerProvider = "github" | "generic";

export interface WebhookTrigger {
  id: string;
  provider: WebhookTriggerProvider;
  workspaceRoot: string;
  prompt: string;
  mode: "agent";
  permissionMode: "ask";
  title: string;
  enabled: boolean;
  /** Redacted readiness flag. The daemon never returns the secret or its value. */
  secretConfigured: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface WebhookTriggerListResponse {
  triggers: WebhookTrigger[];
}

export interface WebhookTriggerDeliveryAuditRow {
  createdAt: string;
  triggerId: string;
  provider: WebhookTriggerProvider;
  deliveryId: string | null;
  outcome: string;
  reason: string;
  sessionId: string | null;
}

export interface WebhookTriggerAuditResponse {
  deliveries: WebhookTriggerDeliveryAuditRow[];
}

export interface CreateWebhookTriggerRequest {
  id?: string;
  provider: WebhookTriggerProvider;
  workspaceRoot: string;
  prompt: string;
  mode?: "agent";
  title?: string;
  /** Environment-variable name only. The secret value never crosses this API. */
  secretEnv: string;
  enabled?: boolean;
}

export interface PatchWebhookTriggerRequest {
  provider?: WebhookTriggerProvider;
  workspaceRoot?: string;
  prompt?: string;
  mode?: "agent";
  title?: string;
  /** Omit to retain the existing environment-variable binding. */
  secretEnv?: string;
  enabled?: boolean;
}

export interface DeleteWebhookTriggerResponse {
  deleted: true;
  id: string;
}
