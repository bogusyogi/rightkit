import { useCallback, useEffect, useRef, useState } from "react";
import {
  createWebhookTrigger,
  deleteWebhookTrigger,
  listWebhookTriggerAudit,
  listWebhookTriggers,
  patchWebhookTrigger,
} from "../../lib/daemon.js";
import type {
  DaemonConfig,
  PatchWebhookTriggerRequest,
  WebhookTrigger,
  WebhookTriggerDeliveryAuditRow,
  WebhookTriggerProvider,
} from "../../lib/types.js";

export interface WebhookTriggerDraft {
  editingId: string | null;
  id: string;
  provider: WebhookTriggerProvider;
  workspaceRoot: string;
  title: string;
  prompt: string;
  /** Environment-variable name only; never the secret value. */
  secretEnv: string;
  enabled: boolean;
}

function emptyDraft(workspaceRoot: string): WebhookTriggerDraft {
  return {
    editingId: null,
    id: "",
    provider: "github",
    workspaceRoot: workspaceRoot.trim(),
    title: "",
    prompt: "Review this webhook event and report the safest next action.",
    secretEnv: "RIGHTKIT_WEBHOOK_SECRET",
    enabled: true,
  };
}

interface UseWebhookTriggersRuntimeParams {
  config: DaemonConfig | null;
  runtimeWorkspace: string;
  onStatus: (status: string) => void;
  formatError: (error: unknown, fallback: string) => string;
}

export function useWebhookTriggersRuntime({
  config,
  runtimeWorkspace,
  onStatus,
  formatError,
}: UseWebhookTriggersRuntimeParams) {
  const [webhookTriggers, setWebhookTriggers] = useState<WebhookTrigger[]>([]);
  const [webhookTriggerAudit, setWebhookTriggerAudit] = useState<WebhookTriggerDeliveryAuditRow[]>(
    [],
  );
  const [webhookTriggerDraft, setWebhookTriggerDraft] = useState<WebhookTriggerDraft>(() =>
    emptyDraft(runtimeWorkspace),
  );
  const [webhookTriggerPending, setWebhookTriggerPending] = useState<string | null>(null);
  const [webhookTriggerLoading, setWebhookTriggerLoading] = useState(false);
  const [webhookTriggerError, setWebhookTriggerError] = useState<string | null>(null);
  const refreshSequence = useRef(0);
  const defaultWorkspaceRef = useRef(runtimeWorkspace.trim());

  useEffect(() => {
    const previousDefault = defaultWorkspaceRef.current;
    const nextDefault = runtimeWorkspace.trim();
    defaultWorkspaceRef.current = nextDefault;
    setWebhookTriggerDraft((draft) => {
      if (
        draft.editingId ||
        (draft.workspaceRoot.trim() && draft.workspaceRoot !== previousDefault)
      ) {
        return draft;
      }
      return { ...draft, workspaceRoot: nextDefault };
    });
  }, [runtimeWorkspace]);

  const refreshWebhookTriggers = useCallback(async () => {
    const sequence = ++refreshSequence.current;
    if (!config) {
      setWebhookTriggers([]);
      setWebhookTriggerAudit([]);
      setWebhookTriggerError(null);
      setWebhookTriggerLoading(false);
      return null;
    }
    setWebhookTriggerLoading(true);
    setWebhookTriggerError(null);
    try {
      const [triggerResponse, auditResponse] = await Promise.all([
        listWebhookTriggers(config),
        listWebhookTriggerAudit(config),
      ]);
      if (sequence !== refreshSequence.current) return triggerResponse;
      setWebhookTriggers(triggerResponse.triggers);
      setWebhookTriggerAudit(auditResponse.deliveries);
      return triggerResponse;
    } catch (error) {
      if (sequence === refreshSequence.current) {
        setWebhookTriggerError(formatError(error, "Webhook triggers failed to load"));
      }
      throw error;
    } finally {
      if (sequence === refreshSequence.current) setWebhookTriggerLoading(false);
    }
  }, [config, formatError]);

  const patchWebhookTriggerDraft = useCallback((patch: Partial<WebhookTriggerDraft>) => {
    setWebhookTriggerDraft((draft) => ({ ...draft, ...patch }));
    setWebhookTriggerError(null);
  }, []);

  const editWebhookTriggerDraft = useCallback((trigger: WebhookTrigger) => {
    setWebhookTriggerDraft({
      editingId: trigger.id,
      id: trigger.id,
      provider: trigger.provider,
      workspaceRoot: trigger.workspaceRoot,
      title: trigger.title,
      prompt: trigger.prompt,
      // The API intentionally redacts the binding name. Blank means retain it.
      secretEnv: "",
      enabled: trigger.enabled,
    });
    setWebhookTriggerError(null);
  }, []);

  const resetWebhookTriggerDraft = useCallback(() => {
    setWebhookTriggerDraft(emptyDraft(runtimeWorkspace));
    setWebhookTriggerError(null);
  }, [runtimeWorkspace]);

  const saveWebhookTriggerDraft = useCallback(async () => {
    if (!config) return;
    const workspaceRoot = webhookTriggerDraft.workspaceRoot.trim();
    const prompt = webhookTriggerDraft.prompt.trim();
    const title = webhookTriggerDraft.title.trim();
    const secretEnv = webhookTriggerDraft.secretEnv.trim();
    if (!workspaceRoot || !prompt) {
      setWebhookTriggerError("Workspace and prompt are required.");
      return;
    }
    if (!webhookTriggerDraft.editingId && !secretEnv) {
      setWebhookTriggerError("A secret environment-variable name is required.");
      return;
    }

    const pendingKey = webhookTriggerDraft.editingId
      ? `edit:${webhookTriggerDraft.editingId}`
      : "create";
    setWebhookTriggerPending(pendingKey);
    setWebhookTriggerError(null);
    try {
      if (webhookTriggerDraft.editingId) {
        const patch: PatchWebhookTriggerRequest = {
          provider: webhookTriggerDraft.provider,
          workspaceRoot,
          prompt,
          mode: "agent",
          title: title || `Webhook: ${webhookTriggerDraft.editingId}`,
          enabled: webhookTriggerDraft.enabled,
          ...(secretEnv ? { secretEnv } : {}),
        };
        await patchWebhookTrigger(config, webhookTriggerDraft.editingId, patch);
      } else {
        await createWebhookTrigger(config, {
          id: webhookTriggerDraft.id.trim() || undefined,
          provider: webhookTriggerDraft.provider,
          workspaceRoot,
          prompt,
          mode: "agent",
          title: title || undefined,
          secretEnv,
          enabled: webhookTriggerDraft.enabled,
        });
      }
      await refreshWebhookTriggers();
      setWebhookTriggerDraft(emptyDraft(runtimeWorkspace));
      onStatus(
        webhookTriggerDraft.editingId ? "Webhook trigger updated" : "Webhook trigger created",
      );
    } catch (error) {
      const message = formatError(
        error,
        webhookTriggerDraft.editingId
          ? "Update webhook trigger failed"
          : "Create webhook trigger failed",
      );
      setWebhookTriggerError(message);
      onStatus(message);
    } finally {
      setWebhookTriggerPending(null);
    }
  }, [
    config,
    formatError,
    onStatus,
    refreshWebhookTriggers,
    runtimeWorkspace,
    webhookTriggerDraft,
  ]);

  const setWebhookTriggerEnabled = useCallback(
    async (id: string, enabled: boolean) => {
      if (!config) return;
      setWebhookTriggerPending(`enabled:${id}`);
      setWebhookTriggerError(null);
      try {
        await patchWebhookTrigger(config, id, { enabled });
        await refreshWebhookTriggers();
        onStatus(enabled ? "Webhook trigger enabled" : "Webhook trigger disabled");
      } catch (error) {
        const message = formatError(error, "Update webhook trigger failed");
        setWebhookTriggerError(message);
        onStatus(message);
      } finally {
        setWebhookTriggerPending(null);
      }
    },
    [config, formatError, onStatus, refreshWebhookTriggers],
  );

  const removeWebhookTrigger = useCallback(
    async (id: string) => {
      if (!config) return;
      setWebhookTriggerPending(`delete:${id}`);
      setWebhookTriggerError(null);
      try {
        await deleteWebhookTrigger(config, id);
        await refreshWebhookTriggers();
        if (webhookTriggerDraft.editingId === id) {
          setWebhookTriggerDraft(emptyDraft(runtimeWorkspace));
        }
        onStatus("Webhook trigger deleted");
      } catch (error) {
        const message = formatError(error, "Delete webhook trigger failed");
        setWebhookTriggerError(message);
        onStatus(message);
      } finally {
        setWebhookTriggerPending(null);
      }
    },
    [
      config,
      formatError,
      onStatus,
      refreshWebhookTriggers,
      runtimeWorkspace,
      webhookTriggerDraft.editingId,
    ],
  );

  return {
    webhookTriggers,
    webhookTriggerAudit,
    webhookTriggerDraft,
    webhookTriggerPending,
    webhookTriggerLoading,
    webhookTriggerError,
    refreshWebhookTriggers,
    patchWebhookTriggerDraft,
    editWebhookTriggerDraft,
    resetWebhookTriggerDraft,
    saveWebhookTriggerDraft,
    setWebhookTriggerEnabled,
    removeWebhookTrigger,
  };
}
