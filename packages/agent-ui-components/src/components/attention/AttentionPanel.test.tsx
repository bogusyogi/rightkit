// D4 — AttentionPanel component tests.

import { describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";

const setStateCalls: Array<{ name: string; args: unknown[] }> = [];

vi.mock("@rightkit/agent-ui-core/state/attentionStore", () => {
  const list = {
    workspaceRoot: "/Users/me/code",
    density: "normal" as const,
    quietHoursActive: false,
    items: [
      {
        id: "a",
        kind: "approval",
        title: "Approve",
        body: "Need your sign-off",
        source: "approval",
        createdAt: "u",
        updatedAt: "u",
        state: "needsInput" as const,
        read: false,
      },
      {
        id: "b",
        kind: "k",
        title: "",
        body: "",
        source: "s",
        createdAt: "u",
        updatedAt: "u",
        state: "unread" as const,
        read: false,
      },
    ],
    counts: {
      unread: 1,
      working: 0,
      needsInput: 1,
      failed: 0,
      complete: 0,
      snoozed: 0,
    },
  };
  const state = {
    list,
    banner: {
      bannerId: "banner-a",
      state: "needsInput" as const,
      title: "Approve",
      body: "Need your sign-off",
      deeplink: { target: "approval", params: { id: "a" } },
      workspaceRoot: "/Users/me/code",
      generatedAt: "u",
    },
    density: { workspaceRoot: "/Users/me/code", density: "normal" as const, durableHistory: true },
    policy: {
      workspaceRoot: "/Users/me/code",
      enabled: true,
      quietProgressSpam: true,
      maxItemsPerSession: 50,
      persistCompleteForHours: 24,
    },
    quietHours: {
      workspaceRoot: "/Users/me/code",
      enabled: false,
      windows: [],
    },
    pending: null,
    error: null,
    loadItems: () => undefined,
    setItemState: (...args: unknown[]) => {
      setStateCalls.push({ name: "setItemState", args });
      return Promise.resolve(true);
    },
    snoozeItem: () => Promise.resolve(true),
    unsnoozeItem: () => Promise.resolve(true),
    loadDensity: () => undefined,
    setDensityMode: () => Promise.resolve(true),
    loadPolicy: () => undefined,
    savePolicy: () => Promise.resolve(true),
    loadQuietHours: () => undefined,
    saveQuietHours: () => Promise.resolve(true),
    resolveDeeplinkTarget: () =>
      Promise.resolve({
        resolved: true,
        target: "approval",
        path: "/attention/approval",
        params: { id: "a" },
      }),
    loadBanner: () => undefined,
    grouped: () => ({}),
    counts: () => list.counts,
    urgent: () => true,
  };
  const useAttentionStore = (selector: (s: typeof state) => unknown) => selector(state);
  useAttentionStore.getState = () => state;
  return { useAttentionStore };
});

import AttentionPanel from "./AttentionPanel.js";

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };

describe("AttentionPanel", () => {
  it("renders the typed state counts and density controls", async () => {
    render(<AttentionPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByTestId("density-summary")).toBeTruthy();
    expect(screen.getByTestId("density-normal")).toBeTruthy();
    expect(screen.getByTestId("density-verbose")).toBeTruthy();
    expect(screen.getByTestId("attention-count-needsInput").textContent).toContain("1");
  });

  it("renders the attention banner when needsInput is present", async () => {
    render(<AttentionPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    const banner = screen.getByTestId("attention-banner");
    expect(banner).toBeTruthy();
    expect(within(banner).getByText(/Approve/)).toBeTruthy();
  });

  it("renders snooze and state controls for each item", async () => {
    render(<AttentionPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByTestId("attention-item-a")).toBeTruthy();
    const item = screen.getByTestId("attention-item-a");
    expect(item.textContent).toMatch(/Snooze 1h/);
    expect(item.textContent).toMatch(/Mark needs input/);
    expect(item.textContent).toMatch(/Mark complete/);
  });

  it("puts notification policy behind a compact settings disclosure", () => {
    render(<AttentionPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByText(/need a decision/i)).toBeTruthy();
    expect((screen.getByTestId("attention-settings") as HTMLDetailsElement).open).toBe(false);
  });

  it("exposes the selected density pill's visible label text and pressed state", () => {
    render(<AttentionPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    const selected = screen.getByTestId("density-normal");
    expect(selected.textContent).toBe("Normal");
    expect(selected.getAttribute("aria-pressed")).toBe("true");

    const unselected = screen.getByTestId("density-summary");
    expect(unselected.textContent).toBe("Summary");
    expect(unselected.getAttribute("aria-pressed")).toBe("false");
  });
});
