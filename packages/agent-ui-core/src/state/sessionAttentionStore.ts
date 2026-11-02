// APP-SES-013 — session attention projection.
//
// Hermes keeps the live dot state separate from the durable unread marker. This
// Zustand port does the same: working/failed/needs-input/background are live
// renderer projections, while unread ids are persisted locally. No selection is
// changed by this store; opening a session is an explicit hub action, which is
// what prevents a background completion from stealing focus.

import { create } from "zustand";

import type { AwaySummary, SessionRow } from "../lib/types.js";

export type SessionAttentionState =
  | "background"
  | "draft"
  | "failed"
  | "idle"
  | "needs-input"
  | "running"
  | "unread";

const STORAGE_KEY = "cr.session-attention.v1";
const MAX_UNREAD = 2_000;

interface PersistedAttention {
  unreadSessionIds: string[];
}

function readPersistedUnread(): Set<string> {
  if (typeof localStorage === "undefined") return new Set();
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as Partial<PersistedAttention> | null;
    if (!parsed || !Array.isArray(parsed.unreadSessionIds)) return new Set();
    return new Set(
      parsed.unreadSessionIds.filter((id): id is string => typeof id === "string" && id.length > 0).slice(-MAX_UNREAD),
    );
  } catch {
    return new Set();
  }
}

function persistUnread(ids: ReadonlySet<string>): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ unreadSessionIds: [...ids].slice(-MAX_UNREAD) }));
  } catch {
    // The in-memory projection remains useful when storage is unavailable.
  }
}

function statusFromRow(status?: string | null): SessionAttentionState {
  const value = (status ?? "").trim().toLowerCase().replaceAll("_", "-");
  if (value.includes("fail") || value.includes("error")) return "failed";
  if (value.includes("needs-input") || value.includes("attention") || value === "blocked") return "needs-input";
  if (value === "running" || value === "working" || value === "in-progress" || value === "active") return "running";
  if (value === "background" || value === "delegating") return "background";
  return "idle";
}

export function attentionStateForSession(
  sessionId: string | null | undefined,
  rowStatus: string | null | undefined,
  overrides: Readonly<Record<string, SessionAttentionState>> = {},
): SessionAttentionState {
  if (!sessionId) return "draft";
  return overrides[sessionId] ?? statusFromRow(rowStatus);
}

export function isActionableAttention(state: SessionAttentionState): boolean {
  return state === "failed" || state === "needs-input";
}

export interface SessionAttentionStoreState {
  bySessionId: Record<string, SessionAttentionState>;
  awaySummaryBySessionId: Record<string, AwaySummary>;
  focusedSessionId: string | null;
  setFocusedSession: (sessionId: string | null) => void;
  syncSessions: (sessions: readonly SessionRow[]) => void;
  setSessionState: (sessionId: string, state: SessionAttentionState) => void;
  markRunning: (sessionId: string) => void;
  markNeedsInput: (sessionId: string) => void;
  markFailed: (sessionId: string) => void;
  markBackground: (sessionId: string) => void;
  markFinished: (sessionId: string, focusedSessionId?: string | null) => void;
  setAwaySummary: (sessionId: string, summary: AwaySummary | null) => void;
  clearAwaySummary: (sessionId: string) => void;
  markUnread: (sessionId: string) => void;
  markRead: (sessionId: string) => void;
  forgetSession: (sessionId: string) => void;
  reset: () => void;
}

const initialUnread = readPersistedUnread();
const initialBySessionId: Record<string, SessionAttentionState> = Object.fromEntries(
  [...initialUnread].map((id) => [id, "unread"]),
);

export const useSessionAttentionStore = create<SessionAttentionStoreState>((set, get) => ({
  bySessionId: initialBySessionId,
  awaySummaryBySessionId: {},
  focusedSessionId: null,

  setFocusedSession(sessionId) {
    set({ focusedSessionId: sessionId });
    if (sessionId) get().markRead(sessionId);
  },

  syncSessions(sessions) {
    const current = get().bySessionId;
    const next = { ...current };
    for (const session of sessions) {
      // A durable unread marker outranks an idle row, but an active live state
      // remains visible until its authoritative terminal event arrives.
      if (current[session.id] === "unread") continue;
      next[session.id] = statusFromRow(session.status);
    }
    set({ bySessionId: next });
  },

  setSessionState(sessionId, state) {
    if (!sessionId) return;
    set((current) => ({ bySessionId: { ...current.bySessionId, [sessionId]: state } }));
  },

  markRunning(sessionId) {
    get().setSessionState(sessionId, "running");
  },

  markNeedsInput(sessionId) {
    get().setSessionState(sessionId, "needs-input");
  },

  markFailed(sessionId) {
    get().setSessionState(sessionId, "failed");
  },

  markBackground(sessionId) {
    get().setSessionState(sessionId, "background");
  },

  markFinished(sessionId, focusedSessionId = get().focusedSessionId) {
    if (!sessionId) return;
    // The caller may pass its authoritative selected id to avoid a stale
    // closure. Finishing the focused session is already observed, so it must
    // not create an unread marker or move focus.
    if (focusedSessionId === sessionId) {
      get().markRead(sessionId);
      return;
    }
    get().markUnread(sessionId);
  },

  setAwaySummary(sessionId, summary) {
    if (!sessionId) return;
    set((current) => {
      const next = { ...current.awaySummaryBySessionId };
      if (summary) next[sessionId] = summary;
      else delete next[sessionId];
      return { awaySummaryBySessionId: next };
    });
  },

  clearAwaySummary(sessionId) {
    get().setAwaySummary(sessionId, null);
  },

  markUnread(sessionId) {
    if (!sessionId) return;
    set((current) => {
      const next = { ...current.bySessionId, [sessionId]: "unread" as const };
      const unread = new Set(Object.entries(next).filter(([, state]) => state === "unread").map(([id]) => id));
      persistUnread(unread);
      return { bySessionId: next };
    });
  },

  markRead(sessionId) {
    if (!sessionId) return;
    set((current) => {
      const previous = current.bySessionId[sessionId];
      const next = { ...current.bySessionId };
      if (previous === "unread") next[sessionId] = "idle";
      const unread = new Set(Object.entries(next).filter(([, state]) => state === "unread").map(([id]) => id));
      persistUnread(unread);
      return { bySessionId: next };
    });
  },

  forgetSession(sessionId) {
    if (!sessionId) return;
    set((current) => {
      const { [sessionId]: _removed, ...next } = current.bySessionId;
      const { [sessionId]: _summaryRemoved, ...awaySummaryBySessionId } = current.awaySummaryBySessionId;
      const unread = new Set(Object.entries(next).filter(([, state]) => state === "unread").map(([id]) => id));
      persistUnread(unread);
      return { awaySummaryBySessionId, bySessionId: next };
    });
  },

  reset() {
    persistUnread(new Set());
    set({ awaySummaryBySessionId: {}, bySessionId: {}, focusedSessionId: null });
  },
}));

// A non-hook alias is useful in event fan-in code and makes the ownership seam
// explicit without requiring a component to subscribe to the whole store.
export const sessionAttentionStore = useSessionAttentionStore;
export { statusFromRow };
