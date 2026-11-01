import { create } from "zustand";
import type { RateLimitEvent } from "../lib/accounts/accounts.js";

type State = {
  /** Latest accountRateLimited event per session. */
  bySession: Record<string, RateLimitEvent>;
  record: (sessionId: string, event: RateLimitEvent) => void;
  clear: (sessionId: string) => void;
};

export const useRateLimitStore = create<State>((set) => ({
  bySession: {},
  record: (sessionId, event) => set((s) => ({ bySession: { ...s.bySession, [sessionId]: event } })),
  clear: (sessionId) =>
    set((s) => {
      const { [sessionId]: _removed, ...rest } = s.bySession;
      return { bySession: rest };
    }),
}));
