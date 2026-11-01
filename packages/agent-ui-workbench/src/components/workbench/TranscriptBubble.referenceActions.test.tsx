import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { useRightPanelStore } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import { TranscriptBubble } from "./TranscriptBubble.js";

describe("TranscriptBubble reference actions", () => {
  beforeEach(() => {
    localStorage.clear();
    useWorkspaceStore.getState().resetForWorkspace("/work/project");
    useRightPanelStore.setState({ tab: "background-tasks" });
    useSidePanelStore.setState({ open: false });
  });

  it("opens a typed workspace document in the real Files viewer", () => {
    const message: TranscriptMessage = {
      $kind: "user",
      id: "message-1",
      createdAt: "2026-07-21T00:00:00Z",
      content: "Read the plan",
      pinned: false,
      attachments: [
        {
          id: "document-1",
          kind: "generated_document",
          destination: "/work/project/docs/plan.md",
          filename: "plan.md",
          provenance: {
            source: "agent",
            createdAt: "2026-07-21T00:00:00Z",
            trust: "workspaceTrusted",
          },
          actions: ["open", "preview"],
        },
      ],
    };

    render(<TranscriptBubble message={message} />);
    fireEvent.click(screen.getByRole("button", { name: "Preview plan.md" }));

    expect(useWorkspaceStore.getState().openedFilePath).toBe("docs/plan.md");
    expect(useRightPanelStore.getState().tab).toBe("files");
    expect(useSidePanelStore.getState().open).toBe(true);
    expect(screen.queryByRole("button", { name: "Remove plan.md" })).toBeNull();
  });
});
