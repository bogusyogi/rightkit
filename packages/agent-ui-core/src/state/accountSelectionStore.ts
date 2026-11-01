import { create } from "zustand";
import {
  fetchAccountSelection,
  putProjectDefaultAccount,
  putSessionAccount,
} from "../lib/accounts/accounts.js";
import type { DaemonConfig } from "../lib/types.js";

export const ACCOUNT_SELECTION_STORAGE_KEY = "cr.accounts.selection.v1";

type Persisted = { chat: Record<string, string>; project: Record<string, string> };

type State = Persisted & {
  /** Cache-only setters. Use selectChatAccount / selectProjectDefault to persist via the daemon. */
  setChatAccount: (chatId: string, accountId: string) => void;
  setProjectDefault: (projectId: string, accountId: string) => void;
  /** Persist through the daemon first; the local cache updates only when the daemon accepts. */
  selectChatAccount: (
    config: DaemonConfig,
    chatId: string,
    accountId: string,
    options?: { threadStarted?: boolean },
  ) => Promise<void>;
  selectProjectDefault: (
    config: DaemonConfig,
    projectId: string,
    accountId: string,
  ) => Promise<void>;
  /** Refresh the cache from the daemon, which is the source of truth. */
  syncFromDaemon: (
    config: DaemonConfig,
    chatId: string | null,
    projectId: string | null,
  ) => Promise<void>;
  /** Chat choice wins, then project default. Never switches automatically. */
  resolveAccount: (chatId: string | null, projectId: string | null) => string | null;
};

function read(): Persisted {
  try {
    const raw =
      typeof localStorage === "undefined"
        ? null
        : localStorage.getItem(ACCOUNT_SELECTION_STORAGE_KEY);
    if (raw) {
      const p = JSON.parse(raw) as Partial<Persisted>;
      return { chat: p.chat ?? {}, project: p.project ?? {} };
    }
  } catch {
    /* fall through */
  }
  return { chat: {}, project: {} };
}

function write(p: Persisted) {
  try {
    localStorage.setItem(ACCOUNT_SELECTION_STORAGE_KEY, JSON.stringify(p));
  } catch {
    /* best effort */
  }
}

function without(map: Record<string, string>, key: string): Record<string, string> {
  const { [key]: _removed, ...rest } = map;
  return rest;
}

export const useAccountSelectionStore = create<State>((set, get) => ({
  ...read(),
  setChatAccount: (chatId, accountId) =>
    set((s) => {
      const next = { chat: { ...s.chat, [chatId]: accountId }, project: s.project };
      write(next);
      return next;
    }),
  setProjectDefault: (projectId, accountId) =>
    set((s) => {
      const next = { chat: s.chat, project: { ...s.project, [projectId]: accountId } };
      write(next);
      return next;
    }),
  selectChatAccount: async (config, chatId, accountId, options) => {
    await putSessionAccount(config, chatId, accountId, options);
    get().setChatAccount(chatId, accountId);
  },
  selectProjectDefault: async (config, projectId, accountId) => {
    await putProjectDefaultAccount(config, projectId, accountId);
    get().setProjectDefault(projectId, accountId);
  },
  syncFromDaemon: async (config, chatId, projectId) => {
    const selection = await fetchAccountSelection(config, {
      sessionId: chatId,
      project: projectId,
    });
    set((s) => {
      let { chat, project } = s;
      // A vanished account resolves to "unavailable", never to another account:
      // drop the cached choice so the picker asks again.
      const id = selection.unavailable ? null : selection.accountId;
      if (chatId) {
        chat = selection.source === "session" && id ? { ...chat, [chatId]: id } : without(chat, chatId);
      }
      if (projectId && selection.source === "project" && id) {
        project = { ...project, [projectId]: id };
      }
      const next = { chat, project };
      write(next);
      return next;
    });
  },
  resolveAccount: (chatId, projectId) =>
    (chatId && get().chat[chatId]) || (projectId && get().project[projectId]) || null,
}));
