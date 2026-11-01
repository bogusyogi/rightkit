// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import ConductorsTab from "./ConductorsTab";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";

function baseSession(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id: "driver-session",
    title: "Codex driver",
    model: "",
    workspaceRoot: "/tmp",
    createdAt: "2026-07-27T00:00:00Z",
    updatedAt: "2026-07-27T00:00:00Z",
    driverKind: "codex",
    ...overrides,
  };
}

describe("ConductorsTab", () => {
  it("renders one card per external CLI with install/auth state and no defaults-for-future-sessions controls", () => {
    render(<ConductorsTab config={null} selectedSession={null} />);

    expect(screen.getByText("Conductors")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Codex" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Claude" })).toBeTruthy();
    expect(screen.getAllByText("Installed").length).toBe(2);
    expect(screen.getAllByText("Authenticated").length).toBe(2);
    expect(screen.getAllByText("Governed tools").length).toBe(2);

    expect(screen.queryByText("Fable mode")).toBeNull();
    expect(screen.queryByText("Execution backend")).toBeNull();
    expect(screen.queryByRole("button", { name: "New execution workspace" })).toBeNull();
  });

  it("prompts to select a Codex/Claude session when none is selected", () => {
    render(<ConductorsTab config={null} selectedSession={null} />);
    expect(screen.getByText(/Select a session that runs on Codex or Claude/)).toBeTruthy();
  });

  it("prompts the same way for a native-driver session", () => {
    render(<ConductorsTab config={null} selectedSession={baseSession({ driverKind: "native" })} />);
    expect(screen.getByText(/Select a session that runs on Codex or Claude/)).toBeTruthy();
  });

  it("shows per-session lifecycle controls for a Codex session", () => {
    render(<ConductorsTab config={null} selectedSession={baseSession({ driverKind: "codex" })} />);
    expect(screen.getByText(/Codex driver/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh binary & auth" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Start" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Resume" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(screen.getByText("Gateway")).toBeTruthy();
    expect(screen.getByText("Vendor session id")).toBeTruthy();
  });

  it("shows per-session lifecycle controls for a Claude session", () => {
    render(
      <ConductorsTab
        config={null}
        selectedSession={baseSession({ id: "s2", title: "Claude driver", driverKind: "claude" })}
      />,
    );
    expect(screen.getByText(/Claude driver/)).toBeTruthy();
  });
});
