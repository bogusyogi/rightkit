import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WorkspaceStartupPanel } from "./WorkspaceStartupPanel";

const startup = {
  output: "install failed",
  entry: {
    root: "/repo",
    fingerprint: "abc",
    state: "approval_required",
    approved: false,
    commands: [
      { id: "deps", label: "Install dependencies", required: true, state: "idle" },
    ],
  },
};

describe("WorkspaceStartupPanel", () => {
  it("renders an interactive approval state for startup commands", () => {
    const html = renderToStaticMarkup(
      <WorkspaceStartupPanel startup={startup} onApprove={() => undefined} onContinue={() => undefined} />,
    );
    expect(html).toContain("Review Workspace Startup");
    expect(html).toContain("Install dependencies");
    expect(html).toContain('role="dialog"');
    expect(html).toContain("Approve and run");
    expect(html).toContain("Skip startup");
  });

  it("labels failures with retry and preserves command output", () => {
    const html = renderToStaticMarkup(
      <WorkspaceStartupPanel
        startup={{
          ...startup,
          output: "command exited 1",
          entry: {
            ...startup.entry,
            state: "failed",
            approved: true,
            reason: "command exited 1",
            commands: [{ ...startup.entry.commands[0], state: "failed" }],
          },
        }}
        onRetry={() => undefined}
        onContinue={() => undefined}
      />,
    );
    expect(html).toContain("Workspace Startup Failed");
    expect(html).toContain("Retry startup");
    expect(html).toContain("command exited 1");
    expect(html).not.toContain(">Continue<");
  });

  it("shows useful progress metadata instead of optional flags", () => {
    const html = renderToStaticMarkup(
      <WorkspaceStartupPanel
        startup={{
          ...startup,
          output: "",
          entry: {
            ...startup.entry,
            state: "running",
            approved: true,
            commands: [
              { id: "aethon-devshell", label: "Prepare environment", required: false, state: "running" },
              { ...startup.entry.commands[0], state: "idle" },
            ],
          },
        }}
      />,
    );
    expect(html).toContain("Prepare environment");
    expect(html).toContain("checking dev shell");
    expect(html).toContain("waiting");
    expect(html).not.toContain("optional");
  });

  it("does not render before a startup entry is available", () => {
    expect(renderToStaticMarkup(<WorkspaceStartupPanel startup={null} />)).toBe("");
  });
});
