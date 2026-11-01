// D4 — Attention store: typed unread / working / needs-input / failed /
// complete / snoozed state, transcript density modes that preserve durable
// history, project notification policy, quiet hours, and deep links.
//
// Reuses the existing /notifications surface for durable storage; adds typed
// state + density + policy on top. Does NOT touch the removed QA bridge, the
// generated client, or `AppBody`.

import { create } from "zustand";

import {
  getAttentionBanner,
  getDensity,
  getNotificationPolicy,
  getQuietHours,
  listAttentionItems,
  resolveDeeplink,
  setDensity,
  setNotificationPolicy,
  setQuietHours,
  snoozeAttentionItem,
  unsnoozeAttentionItem,
  updateAttentionState,
} from "../lib/daemon/delivery-environments-attention";
import {
  groupAttentionByState,
  hasUrgentAttention,
} from "../lib/daemon/delivery-environments-attention";
import type {
  AttentionBanner,
  AttentionCounts,
  AttentionItem,
  AttentionListResponse,
  AttentionState,
  DeeplinkResolution,
  DensityResponse,
  ProjectNotificationPolicy,
  QuietHoursResponse,
  TranscriptDensity,
} from "../lib/types";
import { messageOf } from "../lib/utils";

export type AttentionStoreState = {
  list: AttentionListResponse | null;
  banner: AttentionBanner | null;
  density: DensityResponse | null;
  policy: ProjectNotificationPolicy | null;
  quietHours: QuietHoursResponse | null;
  pending: string | null;
  error: string | null;
  reset: () => void;
  loadItems: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  setItemState: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
    state: AttentionState,
  ) => Promise<boolean>;
  snoozeItem: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
    snoozeUntil: string,
  ) => Promise<boolean>;
  unsnoozeItem: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
  ) => Promise<boolean>;
  loadDensity: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  setDensityMode: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
    density: TranscriptDensity,
  ) => Promise<boolean>;
  loadPolicy: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  savePolicy: (
    config: import("../lib/types").DaemonConfig | null,
    policy: ProjectNotificationPolicy,
  ) => Promise<boolean>;
  loadQuietHours: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  saveQuietHours: (
    config: import("../lib/types").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      enabled: boolean;
      windows: QuietHoursResponse["windows"];
    },
  ) => Promise<boolean>;
  resolveDeeplinkTarget: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
    target: string,
    params?: Record<string, unknown>,
  ) => Promise<DeeplinkResolution | null>;
  loadBanner: (
    config: import("../lib/types").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  grouped: () => Record<AttentionState, AttentionItem[]>;
  counts: () => AttentionCounts;
  urgent: () => boolean;
};

export const useAttentionStore = create<AttentionStoreState>((set, get) => ({
  list: null,
  banner: null,
  density: null,
  policy: null,
  quietHours: null,
  pending: null,
  error: null,
  reset: () =>
    set({
      list: null,
      banner: null,
      density: null,
      policy: null,
      quietHours: null,
      pending: null,
      error: null,
    }),
  async loadItems(config, workspaceRoot) {
    if (!config) return;
    set({ pending: "items", error: null });
    try {
      const list = await listAttentionItems(config, workspaceRoot);
      set({ list, pending: null });
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
    }
  },
  async setItemState(config, workspaceRoot, id, state) {
    if (!config) return false;
    set({ pending: `state:${id}`, error: null });
    try {
      await updateAttentionState(config, workspaceRoot, id, state);
      const current = get().list;
      if (current) {
        const items = current.items.map((row) =>
          row.id === id
            ? { ...row, state, updatedAt: new Date().toISOString(), read: state === "complete" }
            : row,
        );
        const counts: AttentionCounts = {
          unread: 0,
          working: 0,
          needsInput: 0,
          failed: 0,
          complete: 0,
          snoozed: 0,
        };
        for (const item of items) counts[item.state] += 1;
        set({
          pending: null,
          list: { ...current, items, counts },
        });
      } else {
        set({ pending: null });
      }
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async snoozeItem(config, workspaceRoot, id, snoozeUntil) {
    if (!config) return false;
    set({ pending: `snooze:${id}`, error: null });
    try {
      await snoozeAttentionItem(config, workspaceRoot, id, snoozeUntil);
      const current = get().list;
      if (current) {
        const items = current.items.map((row) =>
          row.id === id
            ? { ...row, state: "snoozed" as AttentionState, snoozedUntil: snoozeUntil }
            : row,
        );
        const counts: AttentionCounts = {
          unread: 0,
          working: 0,
          needsInput: 0,
          failed: 0,
          complete: 0,
          snoozed: 0,
        };
        for (const item of items) counts[item.state] += 1;
        set({
          pending: null,
          list: { ...current, items, counts },
        });
      } else {
        set({ pending: null });
      }
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async unsnoozeItem(config, workspaceRoot, id) {
    if (!config) return false;
    set({ pending: `unsnooze:${id}`, error: null });
    try {
      await unsnoozeAttentionItem(config, workspaceRoot, id);
      const current = get().list;
      if (current) {
        const items = current.items.map((row) =>
          row.id === id ? { ...row, state: "unread" as AttentionState, snoozedUntil: null } : row,
        );
        const counts: AttentionCounts = {
          unread: 0,
          working: 0,
          needsInput: 0,
          failed: 0,
          complete: 0,
          snoozed: 0,
        };
        for (const item of items) counts[item.state] += 1;
        set({
          pending: null,
          list: { ...current, items, counts },
        });
      } else {
        set({ pending: null });
      }
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async loadDensity(config, workspaceRoot) {
    if (!config) return;
    set({ pending: "density", error: null });
    try {
      const density = await getDensity(config, workspaceRoot);
      set({ density, pending: null });
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
    }
  },
  async setDensityMode(config, workspaceRoot, density) {
    if (!config) return false;
    set({ pending: "set-density", error: null });
    try {
      const result = await setDensity(config, workspaceRoot, density);
      set({ density: result, pending: null });
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async loadPolicy(config, workspaceRoot) {
    if (!config) return;
    set({ pending: "policy", error: null });
    try {
      const policy = await getNotificationPolicy(config, workspaceRoot);
      set({ policy, pending: null });
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
    }
  },
  async savePolicy(config, policy) {
    if (!config) return false;
    set({ pending: "save-policy", error: null });
    try {
      const saved = await setNotificationPolicy(config, policy);
      set({ policy: saved, pending: null });
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async loadQuietHours(config, workspaceRoot) {
    if (!config) return;
    set({ pending: "quiet-hours", error: null });
    try {
      const quietHours = await getQuietHours(config, workspaceRoot);
      set({ quietHours, pending: null });
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
    }
  },
  async saveQuietHours(config, args) {
    if (!config) return false;
    set({ pending: "save-quiet-hours", error: null });
    try {
      const saved = await setQuietHours(config, args);
      set({ quietHours: saved, pending: null });
      return true;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return false;
    }
  },
  async resolveDeeplinkTarget(config, workspaceRoot, target, params) {
    if (!config) return null;
    set({ pending: "deeplink", error: null });
    try {
      const result = await resolveDeeplink(config, workspaceRoot, target, params);
      set({ pending: null });
      return result;
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
      return null;
    }
  },
  async loadBanner(config, workspaceRoot) {
    if (!config) return;
    set({ pending: "banner", error: null });
    try {
      const banner = await getAttentionBanner(config, workspaceRoot);
      set({ banner: banner ?? null, pending: null });
    } catch (error) {
      set({ pending: null, error: messageOf(error) });
    }
  },
  grouped() {
    const list = get().list;
    if (!list) {
      return {
        unread: [],
        working: [],
        needsInput: [],
        failed: [],
        complete: [],
        snoozed: [],
      } as Record<AttentionState, AttentionItem[]>;
    }
    return groupAttentionByState(list.items);
  },
  counts() {
    return (
      get().list?.counts ?? {
        unread: 0,
        working: 0,
        needsInput: 0,
        failed: 0,
        complete: 0,
        snoozed: 0,
      }
    );
  },
  urgent() {
    const list = get().list;
    if (!list) return false;
    return hasUrgentAttention(list.items);
  },
}));
