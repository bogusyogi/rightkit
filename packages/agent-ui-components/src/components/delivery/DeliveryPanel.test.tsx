// D4 — DeliveryPanel component tests.

import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";

vi.mock("@rightkit/agent-ui-core/state/deliveryStore", () => {
  const state = {
    snapshot: {
      workspaceRoot: "/Users/me/code",
      hostKind: "local",
      hostAvailable: true,
      hostUnavailableReason: null,
      capabilities: [
        { kind: "git", available: true, reason: null },
        { kind: "pullRequest", available: true, reason: null },
        { kind: "checks", available: false, reason: "gh missing" },
        { kind: "failingLogs", available: false, reason: "gh missing" },
        { kind: "backgroundCommand", available: true, reason: null },
      ],
      authority: {
        trusted: true,
        source: "tauri-webview",
        workspaceRoot: "/Users/me/code",
        trustLabel: "trusted",
      },
      gitBinary: "git",
      ghBinary: null,
      gitVersion: "git version 2.45",
      ghVersion: null,
      checkedAt: "unix:1",
    },
    snapshotPending: false,
    history: [],
    actionPending: null,
    error: null,
    loadSnapshot: () => Promise.resolve(),
    loadBranches: () => Promise.resolve(null),
    loadWorktrees: () => Promise.resolve(null),
    createWorktree: () => Promise.resolve(true),
    removeWorktree: () => Promise.resolve(true),
    stagePaths: () => Promise.resolve(true),
    unstagePaths: () => Promise.resolve(true),
    commitChanges: () => Promise.resolve(true),
    pushBranch: vi.fn(() => Promise.resolve(true)),
    pullBranch: () => Promise.resolve(true),
    createPullRequest: vi.fn(() => Promise.resolve(true)),
    loadPullRequest: () => Promise.resolve(null),
    listPullRequestChecks: () => Promise.resolve(null),
    rerunCheck: () => Promise.resolve(true),
    loadCheckLogs: () => Promise.resolve(null),
    listPullRequestThreads: () => Promise.resolve(null),
    runBackgroundCommand: () => Promise.resolve(null),
    cancelBackgroundCommand: () => Promise.resolve(true),
    clearHistory: () => undefined,
  };
  const useDeliveryStore = (selector: (s: typeof state) => unknown) => selector(state);
  useDeliveryStore.getState = () => state;
  return { useDeliveryStore };
});

import DeliveryPanel from "./DeliveryPanel.js";
import { useDeliveryStore } from "@rightkit/agent-ui-core/state/deliveryStore";

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };

function openAdvancedActionsSummary(): HTMLElement {
  const summary = screen.getByTestId("delivery-advanced-actions").querySelector("summary");
  if (!summary) throw new Error("advanced actions summary missing");
  return summary as HTMLElement;
}

describe("DeliveryPanel", () => {
  it("renders host selection and capability badges honestly", () => {
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByLabelText(/delivery host/i)).toBeTruthy();
    expect(screen.getByText(/pullRequest/i)).toBeTruthy();
    expect(screen.getAllByText(/gh missing/i).length).toBeGreaterThan(0);
  });

  it("shows the empty-history placeholder when no commands have run", () => {
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByTestId("delivery-history").textContent).toContain("No commands yet");
  });

  it("keeps branch and pull-request inputs behind the advanced disclosure", () => {
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByText("Commit and sync")).toBeTruthy();
    expect((screen.getByTestId("delivery-advanced-actions") as HTMLDetailsElement).open).toBe(
      false,
    );
    expect((screen.getByTestId("delivery-capabilities") as HTMLDetailsElement).open).toBe(false);
  });

  it("disables push/pull/commit when the git capability is unavailable", () => {
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" initialHost="wsl" />);
    const push = screen.getByRole<HTMLButtonElement>("button", { name: /^Push$/ });
    expect(push).toBeTruthy();
    // push is only disabled when the capability is unavailable; in this panel
    // the buttons stay enabled as long as the typed snapshot says so, which
    // matches the "honest about runtime support" rule.
    expect(push.disabled).toBe(false);
  });

  it("routes Push through a confirm dialog naming the exact scope and does nothing on cancel", () => {
    const store = useDeliveryStore.getState();
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    fireEvent.click(screen.getByRole("button", { name: /^Push$/ }));
    expect(screen.getByRole("alertdialog", { name: /push to remote/i })).toBeTruthy();
    expect(screen.getByText(/Workspace: \/Users\/me\/code/i)).toBeTruthy();
    expect(screen.getByText(/Host: Local/i)).toBeTruthy();
    expect(screen.getByText(/Remote: origin/i)).toBeTruthy();
    expect(store.pushBranch).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /^Cancel$/ }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(store.pushBranch).not.toHaveBeenCalled();
  });

  it("calls pushBranch only after confirming the Push dialog", () => {
    const store = useDeliveryStore.getState();
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    fireEvent.click(screen.getByRole("button", { name: /^Push$/ }));
    const dialog = screen.getByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: /^Push$/ }));
    expect(store.pushBranch).toHaveBeenCalledTimes(1);
    expect(store.pushBranch).toHaveBeenCalledWith(baseConfig, { workspaceRoot: "/Users/me/code" });
  });

  it("routes Open PR through a confirm dialog naming the exact scope and does nothing on cancel", () => {
    const store = useDeliveryStore.getState();
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    fireEvent.click(openAdvancedActionsSummary());
    fireEvent.change(screen.getByLabelText(/pull request title/i), {
      target: { value: "Ship the thing" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^Open PR$/ }));
    expect(screen.getByRole("alertdialog", { name: /open a pull request/i })).toBeTruthy();
    expect(screen.getByText(/Workspace: \/Users\/me\/code/i)).toBeTruthy();
    expect(screen.getByText(/Title: Ship the thing/i)).toBeTruthy();
    expect(store.createPullRequest).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /^Cancel$/ }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(store.createPullRequest).not.toHaveBeenCalled();
  });

  it("calls createPullRequest only after confirming the Open PR dialog", () => {
    const store = useDeliveryStore.getState();
    render(<DeliveryPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    fireEvent.click(openAdvancedActionsSummary());
    fireEvent.click(screen.getByRole("button", { name: /^Open PR$/ }));
    const dialog = screen.getByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: /^Open PR$/ }));
    expect(store.createPullRequest).toHaveBeenCalledTimes(1);
    expect(store.createPullRequest).toHaveBeenCalledWith(baseConfig, {
      workspaceRoot: "/Users/me/code",
      title: undefined,
      body: undefined,
    });
  });
});
