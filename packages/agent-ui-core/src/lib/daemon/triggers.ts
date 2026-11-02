import { requestDaemonApi } from "../generated/daemon-openapi.js";
import type {
  CreateWebhookTriggerRequest,
  DaemonConfig,
  DeleteWebhookTriggerResponse,
  PatchWebhookTriggerRequest,
  WebhookTrigger,
  WebhookTriggerAuditResponse,
  WebhookTriggerListResponse,
} from "../types.js";

export async function listWebhookTriggers(
  config: DaemonConfig,
): Promise<WebhookTriggerListResponse> {
  return requestDaemonApi<WebhookTriggerListResponse>(config, "getTriggers");
}

export async function listWebhookTriggerAudit(
  config: DaemonConfig,
): Promise<WebhookTriggerAuditResponse> {
  return requestDaemonApi<WebhookTriggerAuditResponse>(config, "getTriggersAudit");
}

export async function createWebhookTrigger(
  config: DaemonConfig,
  body: CreateWebhookTriggerRequest,
): Promise<WebhookTrigger> {
  return requestDaemonApi<WebhookTrigger, CreateWebhookTriggerRequest>(config, "postTriggers", {
    body,
  });
}

export async function patchWebhookTrigger(
  config: DaemonConfig,
  id: string,
  body: PatchWebhookTriggerRequest,
): Promise<WebhookTrigger> {
  return requestDaemonApi<WebhookTrigger, PatchWebhookTriggerRequest>(config, "patchTriggersById", {
    pathParams: { id },
    body,
  });
}

export async function deleteWebhookTrigger(
  config: DaemonConfig,
  id: string,
): Promise<DeleteWebhookTriggerResponse> {
  return requestDaemonApi<DeleteWebhookTriggerResponse>(config, "deleteTriggersById", {
    pathParams: { id },
  });
}
