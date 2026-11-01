// @vitest-environment happy-dom
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WebhookTrigger } from "@rightkit/agent-ui-core/lib/types";
import { useWebhookTriggersRuntime } from "@rightkit/agent-ui-core/hooks/runtime/useWebhookTriggersRuntime";

const api = vi.hoisted(() => ({
  list: vi.fn(),
  audit: vi.fn(),
  create: vi.fn(),
  patch: vi.fn(),
  remove: vi.fn(),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  listWebhookTriggers: api.list,
  listWebhookTriggerAudit: api.audit,
  createWebhookTrigger: api.create,
  patchWebhookTrigger: api.patch,
  deleteWebhookTrigger: api.remove,
}));

const trigger: WebhookTrigger = {
  id: "repo-review",
  provider: "github",
  workspaceRoot: "/workspace/repo",
  prompt: "Review the incoming pull request.",
  mode: "agent",
  permissionMode: "ask",
  title: "Repository review",
  enabled: true,
  secretConfigured: true,
  createdAt: "2026-07-17T00:00:00Z",
  updatedAt: "2026-07-17T00:00:00Z",
};

const config = { url: "http://127.0.0.1:5057", token: "daemon-token" };

beforeEach(() => {
  vi.clearAllMocks();
  api.list.mockResolvedValue({ triggers: [trigger] });
  api.audit.mockResolvedValue({ deliveries: [] });
  api.create.mockResolvedValue(trigger);
  api.patch.mockResolvedValue(trigger);
  api.remove.mockResolvedValue({ deleted: true, id: trigger.id });
});

function setup() {
  const onStatus = vi.fn();
  const hook = renderHook(() =>
    useWebhookTriggersRuntime({
      config,
      runtimeWorkspace: "/workspace/repo",
      onStatus,
      formatError: (error, fallback) => (error instanceof Error ? error.message : fallback),
    }),
  );
  return { ...hook, onStatus };
}

describe("useWebhookTriggersRuntime", () => {
  it("loads redacted trigger rows and delivery audit together", async () => {
    const { result } = setup();

    await act(async () => {
      await result.current.refreshWebhookTriggers();
    });

    expect(api.list).toHaveBeenCalledWith(config);
    expect(api.audit).toHaveBeenCalledWith(config);
    expect(result.current.webhookTriggers).toEqual([trigger]);
    expect(result.current.webhookTriggerError).toBeNull();
  });

  it("does not overwrite an existing secret binding when an edit leaves it blank", async () => {
    const { result, onStatus } = setup();

    act(() => result.current.editWebhookTriggerDraft(trigger));
    await waitFor(() => expect(result.current.webhookTriggerDraft.editingId).toBe(trigger.id));
    expect(result.current.webhookTriggerDraft.secretEnv).toBe("");

    await act(async () => {
      await result.current.saveWebhookTriggerDraft();
    });

    expect(api.patch).toHaveBeenCalledWith(
      config,
      trigger.id,
      expect.not.objectContaining({ secretEnv: expect.anything() }),
    );
    expect(onStatus).toHaveBeenCalledWith("Webhook trigger updated");
  });

  it("creates, toggles, and deletes through the daemon API", async () => {
    const { result } = setup();

    await act(async () => {
      await result.current.saveWebhookTriggerDraft();
    });
    expect(api.create).toHaveBeenCalledWith(
      config,
      expect.objectContaining({
        provider: "github",
        workspaceRoot: "/workspace/repo",
        secretEnv: "RIGHTKIT_WEBHOOK_SECRET",
        enabled: true,
      }),
    );

    await act(async () => {
      await result.current.setWebhookTriggerEnabled(trigger.id, false);
      await result.current.removeWebhookTrigger(trigger.id);
    });
    expect(api.patch).toHaveBeenCalledWith(config, trigger.id, { enabled: false });
    expect(api.remove).toHaveBeenCalledWith(config, trigger.id);
  });

  it("keeps create disabled in state until a secret environment-variable name is present", async () => {
    const { result } = setup();
    act(() => result.current.patchWebhookTriggerDraft({ secretEnv: "" }));
    await waitFor(() => expect(result.current.webhookTriggerDraft.secretEnv).toBe(""));

    await act(async () => {
      await result.current.saveWebhookTriggerDraft();
    });

    expect(api.create).not.toHaveBeenCalled();
    expect(result.current.webhookTriggerError).toBe(
      "A secret environment-variable name is required.",
    );
  });
});
