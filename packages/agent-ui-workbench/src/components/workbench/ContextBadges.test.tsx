import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ExecutionBadge, ForkBadge, SafeModeBadge, SpecBadge, ToolsetBadge } from "./ContextBadges";

describe("ToolsetBadge", () => {
  it("renders nothing when there is no active toolset", () => {
    expect(renderToStaticMarkup(<ToolsetBadge />)).toBe("");
    expect(renderToStaticMarkup(<ToolsetBadge toolset="   " />)).toBe("");
  });

  it("shows the trimmed active toolset name", () => {
    const html = renderToStaticMarkup(<ToolsetBadge toolset=" core " />);
    expect(html).toContain("Toolset");
    expect(html).toContain(">core<");
    expect(html).toContain("Active toolset: core");
  });
});

describe("ExecutionBadge", () => {
  it("renders nothing for the default local backend with no workspace", () => {
    expect(renderToStaticMarkup(<ExecutionBadge executionBackend="local" />)).toBe("");
    expect(renderToStaticMarkup(<ExecutionBadge />)).toBe("");
  });

  it("shows a non-local backend name", () => {
    const html = renderToStaticMarkup(<ExecutionBadge executionBackend="docker" />);
    expect(html).toContain("Exec");
    expect(html).toContain(">docker<");
    expect(html).toContain("Execution backend: docker");
  });

  it("falls back to the workspace id label when the backend is local but a workspace is set", () => {
    const html = renderToStaticMarkup(
      <ExecutionBadge executionBackend="local" executionWorkspaceId="ws-1" />,
    );
    expect(html).toContain(">ws-1<");
    expect(html).toContain("Workspace: ws-1");
  });
});

describe("SafeModeBadge", () => {
  it("renders nothing when safe mode is off", () => {
    expect(renderToStaticMarkup(<SafeModeBadge safeMode={false} />)).toBe("");
    expect(renderToStaticMarkup(<SafeModeBadge />)).toBe("");
  });

  it("explains that plugins, hooks, and persistent permission rules are disabled", () => {
    const html = renderToStaticMarkup(<SafeModeBadge safeMode={true} />);
    expect(html).toContain("Safe");
    expect(html).toContain("user plugins, hooks, and persistent permission rules are disabled");
  });
});

describe("ForkBadge", () => {
  it("renders nothing when the session has no fork parent", () => {
    expect(renderToStaticMarkup(<ForkBadge />)).toBe("");
    expect(renderToStaticMarkup(<ForkBadge forkedFromSessionId={null} />)).toBe("");
  });

  it("shows the parent session id when forked", () => {
    const html = renderToStaticMarkup(<ForkBadge forkedFromSessionId="session-42" />);
    expect(html).toContain("Fork");
    expect(html).toContain(">session-42<");
    expect(html).toContain("Forked from session: session-42");
  });
});

describe("SpecBadge", () => {
  it("renders nothing when no spec is bound to the session", () => {
    expect(renderToStaticMarkup(<SpecBadge />)).toBe("");
    expect(renderToStaticMarkup(<SpecBadge specId={null} />)).toBe("");
    expect(renderToStaticMarkup(<SpecBadge specId="   " />)).toBe("");
  });

  it("shows the bound spec id", () => {
    const html = renderToStaticMarkup(<SpecBadge specId="spec-9" />);
    expect(html).toContain("Spec");
    expect(html).toContain(">spec-9<");
    expect(html).toContain("Bound spec: spec-9");
  });
});
