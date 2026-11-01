// D4 — Attention store tests.

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/daemon/delivery-environments-attention", () => ({
  listAttentionItems: () => undefined,
  updateAttentionState: () => undefined,
  snoozeAttentionItem: () => undefined,
  unsnoozeAttentionItem: () => undefined,
  getDensity: () => undefined,
  setDensity: () => undefined,
  getNotificationPolicy: () => undefined,
  setNotificationPolicy: () => undefined,
  getQuietHours: () => undefined,
  setQuietHours: () => undefined,
  resolveDeeplink: () => undefined,
  getAttentionBanner: () => undefined,
  listEnvironments: () => undefined,
  selectEnvironment: () => undefined,
  testEnvironment: () => undefined,
  listSetupScripts: () => undefined,
  runSetupScript: () => undefined,
  cancelSetupScriptRun: () => undefined,
  rerunSetupScript: () => undefined,
  getSetupScriptLogs: () => undefined,
  pickRuntimeSupportedEnvironment: () => null,
  groupAttentionByState: (items: unknown[]) => {
    const map: Record<string, unknown[]> = {
      unread: [],
      working: [],
      needsInput: [],
      failed: [],
      complete: [],
      snoozed: [],
    };
    for (const item of items as Array<{ state: string }>) {
      map[item.state]?.push(item);
    }
    return map as Record<string, unknown[]>;
  },
  hasUrgentAttention: (items: unknown[]) =>
    (items as Array<{ state: string }>).some(
      (item) => item.state === "needsInput" || item.state === "failed",
    ),
  deliveryCapabilityKey: (kind: string) => `capability:${kind}`,
  environmentKindLabel: (kind: string) => kind,
}));

import { useAttentionStore } from "./attentionStore";
import * as client from "../lib/daemon/delivery-environments-attention";
import type {
  AttentionListResponse,
  AttentionState,
  ProjectNotificationPolicy,
} from "../lib/types";

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };
const workspaceRoot = "/Users/me/code";

beforeEach(() => {
  useAttentionStore.getState().reset();
});

describe("attention store", () => {
  it("loads typed items and preserves density", async () => {
    vi.spyOn(client, "listAttentionItems").mockResolvedValueOnce({
      workspaceRoot,
      density: "normal",
      quietHoursActive: false,
      counts: {
        unread: 1,
        working: 0,
        needsInput: 1,
        failed: 0,
        complete: 0,
        snoozed: 0,
      },
      items: [
        {
          id: "a",
          kind: "approval",
          title: "Approve",
          body: "Need your sign-off",
          source: "approval",
          createdAt: "unix:1",
          updatedAt: "unix:1",
          state: "needsInput",
          read: false,
        },
        {
          id: "b",
          kind: "desktopNotification",
          title: "Hello",
          body: "World",
          source: "coderight",
          createdAt: "unix:2",
          updatedAt: "unix:2",
          state: "unread",
          read: false,
        },
      ],
    } satisfies AttentionListResponse);

    await useAttentionStore.getState().loadItems(baseConfig, workspaceRoot);
    const state = useAttentionStore.getState();
    expect(state.list?.density).toBe("normal");
    expect(state.counts().needsInput).toBe(1);
    expect(state.urgent()).toBe(true);
  });

  it("groups items by state", async () => {
    vi.spyOn(client, "listAttentionItems").mockResolvedValueOnce({
      workspaceRoot,
      density: "verbose",
      quietHoursActive: false,
      counts: {
        unread: 0,
        working: 1,
        needsInput: 1,
        failed: 1,
        complete: 0,
        snoozed: 0,
      },
      items: [
        {
          id: "x",
          kind: "k",
          title: "",
          body: "",
          source: "s",
          createdAt: "u",
          updatedAt: "u",
          state: "needsInput",
          read: false,
        },
        {
          id: "y",
          kind: "k",
          title: "",
          body: "",
          source: "s",
          createdAt: "u",
          updatedAt: "u",
          state: "failed",
          read: false,
        },
        {
          id: "z",
          kind: "k",
          title: "",
          body: "",
          source: "s",
          createdAt: "u",
          updatedAt: "u",
          state: "working",
          read: false,
        },
      ],
    } satisfies AttentionListResponse);

    await useAttentionStore.getState().loadItems(baseConfig, workspaceRoot);
    const grouped = useAttentionStore.getState().grouped();
    expect(grouped.needsInput).toHaveLength(1);
    expect(grouped.failed).toHaveLength(1);
    expect(grouped.working).toHaveLength(1);
  });

  it("updates an item's state without dropping history", async () => {
    vi.spyOn(client, "listAttentionItems").mockResolvedValueOnce({
      workspaceRoot,
      density: "normal",
      quietHoursActive: false,
      counts: {
        unread: 1,
        working: 0,
        needsInput: 0,
        failed: 0,
        complete: 0,
        snoozed: 0,
      },
      items: [
        {
          id: "a",
          kind: "k",
          title: "",
          body: "",
          source: "s",
          createdAt: "u",
          updatedAt: "u",
          state: "unread",
          read: false,
        },
      ],
    } satisfies AttentionListResponse);

    vi.spyOn(client, "updateAttentionState").mockResolvedValueOnce({
      id: "a",
      state: "needsInput" satisfies AttentionState,
      updated: true,
      updatedAt: "unix:9",
    });

    await useAttentionStore.getState().loadItems(baseConfig, workspaceRoot);
    const ok = await useAttentionStore
      .getState()
      .setItemState(baseConfig, workspaceRoot, "a", "needsInput");
    expect(ok).toBe(true);
    expect(useAttentionStore.getState().list?.items[0].state).toBe("needsInput");
  });

  it("snoozes an item until a timestamp and counts it", async () => {
    vi.spyOn(client, "listAttentionItems").mockResolvedValueOnce({
      workspaceRoot,
      density: "normal",
      quietHoursActive: false,
      counts: {
        unread: 1,
        working: 0,
        needsInput: 0,
        failed: 0,
        complete: 0,
        snoozed: 0,
      },
      items: [
        {
          id: "a",
          kind: "k",
          title: "",
          body: "",
          source: "s",
          createdAt: "u",
          updatedAt: "u",
          state: "unread",
          read: false,
        },
      ],
    } satisfies AttentionListResponse);

    vi.spyOn(client, "snoozeAttentionItem").mockResolvedValueOnce({
      id: "a",
      snoozedUntil: "unix:999",
      saved: true,
    });

    await useAttentionStore.getState().loadItems(baseConfig, workspaceRoot);
    const ok = await useAttentionStore
      .getState()
      .snoozeItem(baseConfig, workspaceRoot, "a", "unix:999");
    expect(ok).toBe(true);
    expect(useAttentionStore.getState().list?.items[0].state).toBe("snoozed");
    expect(useAttentionStore.getState().counts().snoozed).toBe(1);
  });

  it("persists project notification policy", async () => {
    vi.spyOn(client, "setNotificationPolicy").mockResolvedValueOnce({
      workspaceRoot,
      enabled: false,
      quietProgressSpam: false,
      maxItemsPerSession: 5,
      persistCompleteForHours: 1,
    } satisfies ProjectNotificationPolicy);

    const ok = await useAttentionStore.getState().savePolicy(baseConfig, {
      workspaceRoot,
      enabled: false,
      quietProgressSpam: false,
      maxItemsPerSession: 5,
      persistCompleteForHours: 1,
    });
    expect(ok).toBe(true);
    expect(useAttentionStore.getState().policy?.maxItemsPerSession).toBe(5);
  });

  it("resolves deep links to typed paths", async () => {
    vi.spyOn(client, "resolveDeeplink").mockResolvedValueOnce({
      resolved: true,
      target: "pr",
      path: "/attention/pr",
      params: { id: "42" },
    });
    const result = await useAttentionStore
      .getState()
      .resolveDeeplinkTarget(baseConfig, workspaceRoot, "pr", { id: "42" });
    expect(result?.path).toBe("/attention/pr");
  });
});
