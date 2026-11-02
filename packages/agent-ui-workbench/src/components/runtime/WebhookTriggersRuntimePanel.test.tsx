// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { WebhookTrigger } from "@rightkit/agent-ui-core/lib/types";
import type { WebhookTriggerDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";
import WebhookTriggersRuntimePanel, {
  type WebhookTriggersRuntimePanelProps,
} from "./WebhookTriggersRuntimePanel.js";

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

const draft: WebhookTriggerDraft = {
  editingId: null,
  id: "",
  provider: "github",
  workspaceRoot: "/workspace/repo",
  title: "",
  prompt: "Review this event.",
  secretEnv: "RIGHTKIT_WEBHOOK_SECRET",
  enabled: true,
};

function props(
  overrides: Partial<WebhookTriggersRuntimePanelProps> = {},
): WebhookTriggersRuntimePanelProps {
  return {
    connected: true,
    daemonUrl: "http://127.0.0.1:5057",
    triggers: [],
    audit: [],
    loading: false,
    error: null,
    pendingId: null,
    draft,
    onRefresh: vi.fn(),
    onDraft: vi.fn(),
    onEdit: vi.fn(),
    onResetDraft: vi.fn(),
    onSave: vi.fn(),
    onEnabled: vi.fn(),
    onDelete: vi.fn(),
    ...overrides,
  };
}

afterEach(cleanup);

describe("WebhookTriggersRuntimePanel", () => {
  it("shows the secure empty state and disables creation while disconnected", () => {
    render(<WebhookTriggersRuntimePanel {...props({ connected: false })} />);

    expect(screen.getByRole("status")).toHaveTextContent("Connect the daemon");
    expect(screen.getByText("No webhook triggers")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create trigger" })).toBeDisabled();
    expect(screen.getByText(/never stores or returns its secret value/i)).toBeInTheDocument();
  });

  it("renders endpoint, Ask mode, and redacted environment-secret readiness", () => {
    render(<WebhookTriggersRuntimePanel {...props({ triggers: [trigger] })} />);

    expect(screen.getByText("Repository review")).toBeInTheDocument();
    expect(screen.getByText("Ask mode")).toBeInTheDocument();
    expect(screen.getByText("Env secret ready")).toBeInTheDocument();
    expect(
      screen.getByTitle("http://127.0.0.1:5057/triggers/repo-review/deliver/github"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/super-secret-value/i)).not.toBeInTheDocument();
  });

  it("routes edit, disable, and confirmed delete actions to their callbacks", () => {
    const onEdit = vi.fn();
    const onEnabled = vi.fn();
    const onDelete = vi.fn();
    render(
      <WebhookTriggersRuntimePanel
        {...props({ triggers: [trigger], onEdit, onEnabled, onDelete })}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Edit repo-review" }));
    fireEvent.click(screen.getByRole("button", { name: "Disable" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete repo-review" }));
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Delete trigger" }));

    expect(onEdit).toHaveBeenCalledWith(trigger);
    expect(onEnabled).toHaveBeenCalledWith("repo-review", false);
    expect(onDelete).toHaveBeenCalledWith("repo-review");
  });

  it("shows delivery outcomes without payload content", () => {
    render(
      <WebhookTriggersRuntimePanel
        {...props({
          triggers: [trigger],
          audit: [
            {
              createdAt: "2026-07-17T12:00:00Z",
              triggerId: "repo-review",
              provider: "github",
              deliveryId: "delivery-1",
              outcome: "accepted",
              reason: "session_submitted",
              sessionId: "trigger-repo-review-abc",
            },
          ],
        })}
      />,
    );

    expect(screen.getByText("accepted")).toBeInTheDocument();
    expect(screen.getByText("session_submitted")).toBeInTheDocument();
    expect(screen.getByText("trigger-repo-review-abc")).toBeInTheDocument();
    expect(screen.getByText("Payloads and secrets are redacted")).toBeInTheDocument();
  });
});
