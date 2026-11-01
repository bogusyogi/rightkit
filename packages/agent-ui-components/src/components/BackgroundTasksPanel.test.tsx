// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import BackgroundTasksPanel from "./BackgroundTasksPanel";

const agents: SubAgentState[] = [
  {
    subSessionId: "running-1",
    role: "security",
    taskTitle: "Audit trust boundaries",
    status: "running",
    startedAt: 1,
    tokensIn: 12_000,
    tokensOut: 500,
    toolUses: 7,
    elapsedMs: 44_000,
    lastTools: ["rg", "cargo test"],
    summary: "Inspecting the daemon boundary.",
  },
  {
    subSessionId: "done-1",
    role: "qa",
    taskTitle: "Verify installed UI",
    status: "completed",
    startedAt: 1,
    success: true,
    tokensIn: 4_000,
    tokensOut: 200,
    toolUses: 3,
    elapsedMs: 20_000,
  },
];

afterEach(cleanup);

describe("BackgroundTasksPanel", () => {
  it("renders a compact agent inspector without workflow-dashboard chrome", () => {
    render(
      <BackgroundTasksPanel
        agents={agents}
        sessionTitle="Harden external intelligence seams"
        goalStatus="active"
        costUsd={1.2847}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.getByRole("complementary", { name: "Agents" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Running" })).toBeTruthy();
    expect(screen.getByText("Audit trust boundaries")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop Audit trust boundaries" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Recent" })).toBeTruthy();
    expect(screen.queryByText("Run stages")).toBeNull();
    expect(screen.queryByText("Parallel work")).toBeNull();
    expect(screen.queryByText("Synthesis")).toBeNull();
    expect(screen.queryByText("Cost")).toBeNull();
  });

  it("reveals evidence and opens the durable child transcript", () => {
    const onOpenAgentSession = vi.fn();
    render(
      <BackgroundTasksPanel
        agents={agents}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={onOpenAgentSession}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Show details for Audit trust boundaries" }),
    );
    expect(screen.getByText("Inspecting the daemon boundary.")).toBeTruthy();
    expect(screen.getByText("cargo test")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "View transcript" }));
    expect(onOpenAgentSession).toHaveBeenCalledWith("running-1");
  });

  it("keeps per-agent and stop-all cancellation controls", () => {
    const onStop = vi.fn();
    const onCancelAll = vi.fn();
    render(
      <BackgroundTasksPanel
        agents={[
          agents[0],
          { ...agents[0], subSessionId: "running-2", taskTitle: "Audit storage" },
        ]}
        onClose={() => {}}
        onStop={onStop}
        onCancelAll={onCancelAll}
        onOpenAgentSession={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Stop Audit trust boundaries" }));
    fireEvent.click(screen.getByRole("button", { name: "Stop all running agents" }));
    expect(onStop).toHaveBeenCalledWith("running-1");
    expect(onCancelAll).toHaveBeenCalledOnce();
  });

  it("keeps Agents maximize and restore controls", () => {
    const onMaximize = vi.fn();
    const { rerender } = render(
      <BackgroundTasksPanel
        agents={[]}
        onClose={() => {}}
        onMaximize={onMaximize}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Maximize Agents panel" }));
    expect(onMaximize).toHaveBeenCalledOnce();

    rerender(
      <BackgroundTasksPanel
        agents={[]}
        maximized
        onClose={() => {}}
        onMaximize={onMaximize}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );
    expect(screen.getByRole("button", { name: "Restore Agents panel" })).toBeTruthy();
  });

  it("renders a calm empty state without fake workflow metrics", () => {
    render(
      <BackgroundTasksPanel
        agents={[]}
        sessionTitle="Empty rail"
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.getByTestId("agents-no-running").textContent).toContain("No agents running.");
    expect(screen.getByTestId("agents-no-running").textContent).toContain(
      "Agents appear here when work is delegated.",
    );
    expect(screen.queryByText("Run stages")).toBeNull();
  });

  it("does not declare an Idle agent state type or per-agent idle section", () => {
    const { container } = render(
      <BackgroundTasksPanel
        agents={[]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );
    // No rendered "Idle" h2/section dedicated to per-agent idle rows.
    expect(container.querySelector("[data-testid='agents-idle']")).toBeNull();
    expect(container.querySelector("[id='idle-agents-title']")).toBeNull();
  });

  it("keeps finished agents collapsed under Recent", () => {
    render(
      <BackgroundTasksPanel
        agents={[agents[1]]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.queryByText("Verify installed UI")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Recent/ }));
    expect(screen.getByText("Verify installed UI")).toBeTruthy();
  });
});

describe("BackgroundTasksPanel agent hierarchy", () => {
  const conductor: SubAgentState = {
    subSessionId: "conductor-1",
    role: "conductor",
    taskTitle: "Coordinate the audit",
    model: "opus-4",
    status: "running",
    startedAt: 1,
  };

  it("nests a child agent under its parent using parentSubSessionId", () => {
    const child: SubAgentState = {
      subSessionId: "agent-1",
      role: "security",
      taskTitle: "Scan for secrets",
      status: "running",
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    };
    render(
      <BackgroundTasksPanel
        agents={[conductor, child]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    const group = document.getElementById("agent-children-conductor-1");
    expect(group).toBeTruthy();
    expect(group?.textContent).toContain("Scan for secrets");
  });

  it("renders deep nesting: conductor -> agent -> subagent", () => {
    const agent: SubAgentState = {
      subSessionId: "agent-1",
      role: "security",
      taskTitle: "Scan for secrets",
      status: "running",
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    };
    const subAgent: SubAgentState = {
      subSessionId: "subagent-1",
      role: "grep-worker",
      taskTitle: "Grep for API keys",
      status: "running",
      startedAt: 1,
      parentSubSessionId: "agent-1",
    };
    render(
      <BackgroundTasksPanel
        agents={[conductor, agent, subAgent]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    const conductorGroup = document.getElementById("agent-children-conductor-1");
    const agentGroup = document.getElementById("agent-children-agent-1");
    expect(agentGroup).toBeTruthy();
    expect(conductorGroup?.contains(agentGroup)).toBe(true);
    expect(agentGroup?.textContent).toContain("Grep for API keys");
  });

  it("shows model and status per node across the hierarchy", () => {
    const child: SubAgentState = {
      subSessionId: "agent-1",
      role: "security",
      taskTitle: "Scan for secrets",
      model: "haiku-4",
      status: "completed",
      success: true,
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    };
    render(
      <BackgroundTasksPanel
        agents={[conductor, child]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.getByText("opus-4")).toBeTruthy();
    expect(screen.getByText("haiku-4")).toBeTruthy();
    expect(screen.getByText("Succeeded")).toBeTruthy();
  });

  it("marks a failed child agent as needing attention", () => {
    const child: SubAgentState = {
      subSessionId: "agent-1",
      role: "security",
      taskTitle: "Scan for secrets",
      status: "completed",
      success: false,
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    };
    render(
      <BackgroundTasksPanel
        agents={[conductor, child]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.getByTestId("agent-attention-agent-1")).toBeTruthy();
    expect(screen.getByText("Needs attention")).toBeTruthy();
  });

  it("collapses a group of more than 3 children by default and expands on click", () => {
    const children: SubAgentState[] = Array.from({ length: 4 }, (_, i) => ({
      subSessionId: `agent-${i}`,
      role: "worker",
      taskTitle: `Worker task ${i}`,
      status: "running" as const,
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    }));
    render(
      <BackgroundTasksPanel
        agents={[conductor, ...children]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    const toggle = screen.getByRole("button", {
      name: "Show 4 sub-agents of Coordinate the audit",
    });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Worker task 0")).toBeNull();

    fireEvent.click(toggle);

    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Worker task 0")).toBeTruthy();
    expect(screen.getByText("Worker task 3")).toBeTruthy();
  });

  it("does not collapse a group of 3 or fewer children", () => {
    const children: SubAgentState[] = Array.from({ length: 3 }, (_, i) => ({
      subSessionId: `agent-${i}`,
      role: "worker",
      taskTitle: `Worker task ${i}`,
      status: "running" as const,
      startedAt: 1,
      parentSubSessionId: "conductor-1",
    }));
    render(
      <BackgroundTasksPanel
        agents={[conductor, ...children]}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(screen.queryByRole("button", { name: /Show \d+ sub-agents/ })).toBeNull();
    expect(screen.getByText("Worker task 0")).toBeTruthy();
    expect(screen.getByText("Worker task 2")).toBeTruthy();
  });

  it("falls back to a flat list when no parentSubSessionId is present", () => {
    const flatAgents: SubAgentState[] = [
      conductor,
      {
        subSessionId: "agent-1",
        role: "security",
        taskTitle: "Scan for secrets",
        status: "running",
        startedAt: 1,
      },
    ];
    render(
      <BackgroundTasksPanel
        agents={flatAgents}
        onClose={() => {}}
        onStop={() => {}}
        onOpenAgentSession={() => {}}
      />,
    );

    expect(document.getElementById("agent-children-conductor-1")).toBeNull();
    expect(screen.getByText("Scan for secrets")).toBeTruthy();
  });
});
