// apps/coderight-tauri/src/components/workbench/PermissionCard.test.tsx
// Phase 5 — extraction + a11y tightening test. Asserts the card is a labeled
// region (not a modal/dialog) and renders inside the message stream.

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import PermissionCard from "./PermissionCard.js";

const basePending = {
  id: "a1",
  toolCallId: "tc1",
  summary: "Allow shell command `cargo test`?",
  decision: "ask" as const,
};

const baseSession = {
  id: "s1",
  title: "Demo session",
  workspaceRoot: "/tmp",
  model: "moonshotai/kimi-k2.6",
  status: "active" as const,
  createdAt: new Date().toISOString(),
  updatedAt: new Date().toISOString(),
};

describe("PermissionCard", () => {
  it("renders as a region with aria-label 'Permission request'", () => {
    const html = renderToStaticMarkup(
      createElement(PermissionCard, {
        pendingApproval: basePending,
        selected: baseSession,
        onApprove: () => undefined,
        onDeny: () => undefined,
        onStageHunk: () => undefined,
      }),
    );
    expect(html).toContain("<section");
    expect(html).toContain('aria-label="Permission request"');
  });

  it("does not use role=dialog (not a modal)", () => {
    const html = renderToStaticMarkup(
      createElement(PermissionCard, {
        pendingApproval: basePending,
        selected: baseSession,
        onApprove: () => undefined,
        onDeny: () => undefined,
        onStageHunk: () => undefined,
      }),
    );
    expect(html).not.toContain('role="dialog"');
  });

  it("renders the summary text and the Allow / Deny buttons", () => {
    const html = renderToStaticMarkup(
      createElement(PermissionCard, {
        pendingApproval: basePending,
        selected: baseSession,
        onApprove: () => undefined,
        onDeny: () => undefined,
        onStageHunk: () => undefined,
      }),
    );
    expect(html).toContain("Allow shell command `cargo test`?");
    expect(html).toContain("Allow");
    expect(html).toContain("Deny");
  });
});
