import { create } from "zustand";

export const THREAD_GOAL_STATUSES = [
  "active",
  "paused",
  "blocked",
  "completed",
  "failed",
  "cancelled",
] as const;

export type ThreadGoalStatus = (typeof THREAD_GOAL_STATUSES)[number];

export type ThreadGoal = {
  id: string;
  threadId: string;
  objective: string;
  status: ThreadGoalStatus;
  detail?: string;
  tokenBudget?: number;
  tokensUsed?: number;
  timeUsedSeconds?: number;
};

type GoalRecord = Record<string, unknown>;

const STATUS_ALIASES: Record<string, ThreadGoalStatus> = {
  active: "active",
  in_progress: "active",
  pending: "active",
  running: "active",
  paused: "paused",
  blocked: "blocked",
  achieved: "completed",
  complete: "completed",
  completed: "completed",
  done: "completed",
  failed: "failed",
  unmet: "failed",
  error: "failed",
  cancel: "cancelled",
  cancelled: "cancelled",
  canceled: "cancelled",
  interrupted: "cancelled",
  stopped: "cancelled",
};

function asRecord(value: unknown): GoalRecord | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as GoalRecord)
    : null;
}

function stringValue(record: GoalRecord, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return undefined;
}

function numberValue(record: GoalRecord, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const value = record[key];
    const number =
      typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
    if (Number.isFinite(number) && number >= 0) return number;
  }
  return undefined;
}

function normalizeStatus(value: unknown): ThreadGoalStatus | null {
  if (typeof value !== "string") return null;
  return (
    STATUS_ALIASES[
      value
        .trim()
        .toLowerCase()
        .replace(/[\s-]+/g, "_")
    ] ?? null
  );
}

function recordsFromPayload(payload: unknown): unknown[] {
  if (Array.isArray(payload)) return payload;
  const record = asRecord(payload);
  if (!record) return [];
  for (const key of ["goals", "items", "data"]) {
    if (Array.isArray(record[key])) return record[key];
  }
  return [record];
}

function parseThreadGoal(value: unknown, fallbackThreadId?: string): ThreadGoal | null {
  const record = asRecord(value);
  if (!record) return null;

  const id = stringValue(record, "id", "goalId", "goal_id");
  const threadId =
    stringValue(record, "threadId", "thread_id", "sessionId", "session_id") ?? fallbackThreadId;
  const objective = stringValue(record, "objective", "text", "title");
  const status = normalizeStatus(record.status);
  if (!id || !threadId || !objective || !status) return null;

  const detail = stringValue(record, "detail", "note", "reason", "evidence");
  const tokenBudget = numberValue(
    record,
    "tokenBudget",
    "token_budget",
    "budgetTokens",
    "budget_tokens",
  );
  const tokensUsed = numberValue(record, "tokensUsed", "tokens_used");
  const timeUsedSeconds = numberValue(record, "timeUsedSeconds", "time_used_seconds");

  return {
    id,
    threadId,
    objective,
    status,
    ...(detail ? { detail } : {}),
    ...(tokenBudget === undefined ? {} : { tokenBudget }),
    ...(tokensUsed === undefined ? {} : { tokensUsed }),
    ...(timeUsedSeconds === undefined ? {} : { timeUsedSeconds }),
  };
}

/** Converts daemon/legacy goal payloads at the boundary into the shell's stable model. */
export function parseThreadGoals(payload: unknown, fallbackThreadId?: string): ThreadGoal[] {
  return recordsFromPayload(payload).flatMap((value) => {
    const goal = parseThreadGoal(value, fallbackThreadId);
    return goal ? [goal] : [];
  });
}

export type GoalsStoreState = {
  goalsByThread: Record<string, ThreadGoal[]>;
  goalsForThread: (threadId: string) => ThreadGoal[];
  replaceThreadGoals: (threadId: string, payload: unknown) => void;
  reset: () => void;
};

export const useGoalsStore = create<GoalsStoreState>((set, get) => ({
  goalsByThread: {},
  goalsForThread(threadId) {
    return get().goalsByThread[threadId] ?? [];
  },
  replaceThreadGoals(threadId, payload) {
    set((state) => ({
      goalsByThread: { ...state.goalsByThread, [threadId]: parseThreadGoals(payload, threadId) },
    }));
  },
  reset() {
    set({ goalsByThread: {} });
  },
}));
