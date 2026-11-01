// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import RuntimeMiddlewareTab from "./RuntimeMiddlewareTab";
import type { MiddlewareRegistrySnapshot } from "@rightkit/agent-ui-core/lib/types";

// PA-07(c): `tool` / `requiresToolset` are free-form daemon-supplied strings
// (real MCP tool ids run 60-80+ chars with no break points, e.g.
// "mcp__0e71d0c2-64ec-439b-be59-0bff867c72ee__brand-radar-mentions-history-entities").
// As a single unbreakable token inside a flex-wrap badge row, that string's
// intrinsic min-content width blew out past the container's right edge instead
// of wrapping. Fix: min-w-0 + max-w-full + truncate on the badge, with the
// full value recoverable via title.
const LONG_TOOL_NAME =
  "mcp__0e71d0c2-64ec-439b-be59-0bff867c72ee__brand-radar-mentions-history-entities";

function snapshot(overrides: Partial<MiddlewareRegistrySnapshot> = {}): MiddlewareRegistrySnapshot {
  return {
    middleware: [
      {
        id: "mw-1",
        title: "Approval gate",
        source: "builtIn",
        phase: "preToolCall",
        effect: "requireApproval",
        tool: LONG_TOOL_NAME,
        requiresToolset: LONG_TOOL_NAME,
        enabled: true,
        order: 0,
        description: "Requires human approval before invoking a mutating tool.",
      },
    ],
    spans: [],
    observations: [],
    ...overrides,
  };
}

describe("RuntimeMiddlewareTab narrow-width layout (PA-07c)", () => {
  it("truncates a long unbreakable tool id instead of overflowing its badge", () => {
    render(
      <RuntimeMiddlewareTab
        middlewareSnapshot={snapshot()}
        pendingId={null}
        error={null}
        onRefresh={vi.fn()}
        onToggle={vi.fn()}
      />,
    );

    const toolBadge = screen.getByTitle(`tool ${LONG_TOOL_NAME}`);
    expect(toolBadge.className).toContain("truncate");
    expect(toolBadge.className).toContain("min-w-0");
    expect(toolBadge.className).toContain("max-w-full");
    expect(toolBadge.textContent).toBe(`tool ${LONG_TOOL_NAME}`);

    const toolsetBadge = screen.getByTitle(`toolset ${LONG_TOOL_NAME}`);
    expect(toolsetBadge.className).toContain("truncate");
    expect(toolsetBadge.className).toContain("min-w-0");
    expect(toolsetBadge.className).toContain("max-w-full");
  });

  it("keeps the full middleware title and description recoverable via title when they truncate/clamp", () => {
    render(
      <RuntimeMiddlewareTab
        middlewareSnapshot={snapshot()}
        pendingId={null}
        error={null}
        onRefresh={vi.fn()}
        onToggle={vi.fn()}
      />,
    );

    const title = screen.getByText("Approval gate");
    expect(title.getAttribute("title")).toBe("Approval gate");

    const description = screen.getByText(
      "Requires human approval before invoking a mutating tool.",
    );
    expect(description.getAttribute("title")).toBe(
      "Requires human approval before invoking a mutating tool.",
    );
  });

  it("keeps the middleware row inside a shrinkable grid column", () => {
    const { container } = render(
      <RuntimeMiddlewareTab
        middlewareSnapshot={snapshot()}
        pendingId={null}
        error={null}
        onRefresh={vi.fn()}
        onToggle={vi.fn()}
      />,
    );
    const article = container.querySelector("article");
    expect(article?.className).toContain("grid-cols-[minmax(0,1fr)_auto]");
  });
});
