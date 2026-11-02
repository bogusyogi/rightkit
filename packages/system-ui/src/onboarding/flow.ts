import { invoke } from "@tauri-apps/api/core";
import { onboardingSettings } from "../lib/onboardingSettings.js";

// The window's own progress, kept in the hub's `hub-onboarding.json` document beside the shared component's record
// (`onboarding.state`: step, completed, a copy of the legal receipt). One versioned object holds the draft choices, the access
// choices and the commit status, saved after every change, so closing the window or restarting loses nothing.
export const FLOW_KEY = "pulseFlow";
export const FLOW_VERSION = 1;

export type Draft = Record<string, boolean | string>;

export interface Commit {
  /** `writing` was saved before the first write: a window that finds it after a restart treats it as unfinished. */
  status: "idle" | "writing" | "partial" | "done";
  /** Setting key to why its write was not confirmed. */
  failed: Record<string, string>;
}

export interface Flow {
  version: typeof FLOW_VERSION;
  /** A first run starts every switch on; a re-run starts from the current values. Fixed when the flow is made. */
  first: boolean;
  draft: Draft;
  /** Permission ids the user chose to decide later. */
  deferred: string[];
  commit: Commit;
}

export const freshFlow = (first: boolean): Flow => ({ version: FLOW_VERSION, first, draft: {}, deferred: [], commit: { status: "idle", failed: {} } });

export async function loadFlow(): Promise<Flow | null> {
  try {
    const snapshot = await onboardingSettings.load();
    const raw = snapshot.values[FLOW_KEY] as Partial<Flow> | undefined;
    if (!raw || raw.version !== FLOW_VERSION || typeof raw.first !== "boolean") return null;
    const commit = raw.commit && typeof raw.commit === "object" ? raw.commit : undefined;
    return {
      version: FLOW_VERSION,
      first: raw.first,
      draft: raw.draft && typeof raw.draft === "object" ? raw.draft : {},
      deferred: Array.isArray(raw.deferred) ? raw.deferred.filter((id): id is string => typeof id === "string") : [],
      // A run interrupted mid-write is unfinished, not done.
      commit: { status: commit?.status === "done" ? "done" : commit?.status === "partial" ? "partial" : "idle", failed: commit?.failed ?? {} },
    };
  } catch {
    return null;
  }
}

export async function saveFlow(flow: Flow): Promise<void> {
  await onboardingSettings.patch({ [FLOW_KEY]: flow });
}

// ---- Writing the choices to the notch ------------------------------------------------------------------------------

type SettingValue = boolean | string;
type Published = { settings?: Record<string, unknown> };
type Answer = { status?: string; reason?: string };

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function readSettings(): Promise<Record<string, unknown> | null> {
  try {
    return ((await invoke<Published>("notch_state")).settings ?? {}) as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** Waits for the notch to publish `key` at `value`: the proof for an answer that was only "pending". */
async function published(key: string, value: SettingValue): Promise<boolean> {
  for (let attempt = 0; attempt < 8; attempt++) {
    const settings = await readSettings();
    if (settings && settings[key] === value) return true;
    await sleep(600);
  }
  return false;
}

/** One confirmed write: the notch's typed answer, or, when it has not answered yet, its published state. */
export async function writeSetting(key: string, value: SettingValue): Promise<string | null> {
  let answer: Answer;
  try {
    answer = await invoke<Answer>("notch_command", { command: { command: "set", key, value } });
  } catch (error) {
    return String(error);
  }
  if (answer?.status === "rejected" || answer?.status === "not_saved") return answer.reason ?? answer.status;
  if (answer?.status === "committed" || answer?.status === "unchanged" || answer?.status === "applied") return null;
  return (await published(key, value)) ? null : "the notch did not confirm the change";
}

/**
 * Writes each `[key, value]` that the notch does not already hold, one at a time, and returns the keys whose write was
 * not confirmed. Nothing is assumed: the next call reads the published state again, so a retry repeats only what failed.
 */
export async function writeAll(plan: [string, SettingValue][]): Promise<Record<string, string>> {
  const failed: Record<string, string> = {};
  const current = (await readSettings()) ?? {};
  for (const [key, value] of plan) {
    if (current[key] === value) continue;
    const reason = await writeSetting(key, value);
    if (reason) failed[key] = reason;
  }
  return failed;
}
