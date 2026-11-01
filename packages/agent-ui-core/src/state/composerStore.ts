import { create } from "zustand";

import type { Attachment } from "../lib/daemon";
import { useReferenceStore } from "./referenceStore";

/** Composer permission mode (Phase 4 visual layer — Phase 4.5 will wire this
 * to the daemon's `session.mode` for actual tool enforcement). */
export type ComposerMode = "read" | "edit" | "plan";
/** Claude-Code parity: 5 mode pills (Manual / Accept edits / Plan / Auto / Bypass). */
export type ComposerAgentMode = "manual" | "accept-edits" | "plan" | "auto" | "bypass-permissions";
/** Composer model picker. The label is what shows in the chip; the rank
 * controls the visual ordering in the popover (lower = higher). */
export type ComposerModelKey = "fable-5" | "opus-4.8" | "sonnet-5" | "haiku-4.5";
/** Run-target scope for the composer (Local = local daemon, Cloud = remote). */
export type ComposerScope = "local" | "cloud";
/** Effort tier in the model popover slider. */
export type ComposerEffort = "faster" | "balanced" | "deeper" | "smarter";

export const COMPOSER_MODE_STORAGE_KEY = "cr.composerMode.v1";
const COMPOSER_MODES: readonly ComposerMode[] = ["read", "edit", "plan"];

const AGENT_MODES: readonly ComposerAgentMode[] = [
  "manual",
  "accept-edits",
  "plan",
  "auto",
  "bypass-permissions",
];

function isComposerMode(value: unknown): value is ComposerMode {
  return typeof value === "string" && (COMPOSER_MODES as readonly string[]).includes(value);
}

function isAgentMode(value: unknown): value is ComposerAgentMode {
  return typeof value === "string" && (AGENT_MODES as readonly string[]).includes(value);
}

function readPersistedComposerMode(): ComposerMode {
  if (typeof localStorage === "undefined") return "edit";
  try {
    const stored = localStorage.getItem(COMPOSER_MODE_STORAGE_KEY);
    return isComposerMode(stored) ? stored : "edit";
  } catch {
    return "edit";
  }
}

function persistComposerMode(mode: ComposerMode) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(COMPOSER_MODE_STORAGE_KEY, mode);
  } catch {
    // localStorage may be unavailable (private mode, quota); fall through silently.
  }
}

/** Composer input state: current text, attachment list, and the prompt-history
 * ring with its navigation cursor. */
export type ComposerStoreState = {
  composer: string;
  promptHistory: string[];
  promptHistoryIndex: number | null;
  promptHistoryDraft: string;
  attachments: Attachment[];
  mode: ComposerMode;
  /** Claude-Code parity: 5 mode pills (Manual / Accept edits / Plan / Auto / Bypass). */
  agentMode: ComposerAgentMode;
  /** Selected model key — drives the model chip + Model popover selection. */
  model: ComposerModelKey;
  fastMode: boolean;
  /** Run scope: local daemon or cloud (visual placeholder for now). */
  scope: ComposerScope;
  /** Project / worktree name; rendered as the chip when non-empty. */
  worktree: string;
  /** Effort tier used by the Model popover slider. */
  effort: ComposerEffort;
  setComposer: (value: string) => void;
  setAttachments: (attachments: Attachment[]) => void;
  addAttachments: (attachments: Attachment[]) => void;
  removeAttachment: (index: number) => void;
  clear: () => void;
  rememberPrompt: (value: string) => void;
  changeComposer: (value: string) => void;
  navigateHistory: (direction: -1 | 1) => void;
  setMode: (mode: ComposerMode) => void;
  setAgentMode: (mode: ComposerAgentMode) => void;
  setModel: (model: ComposerModelKey) => void;
  setFastMode: (value: boolean) => void;
  setScope: (scope: ComposerScope) => void;
  setWorktree: (worktree: string) => void;
  setEffort: (effort: ComposerEffort) => void;
};

export const useComposerStore = create<ComposerStoreState>((set, get) => ({
  composer: "",
  promptHistory: [],
  promptHistoryIndex: null,
  promptHistoryDraft: "",
  attachments: [],
  mode: readPersistedComposerMode(),
  agentMode: "accept-edits",
  model: "opus-4.8",
  fastMode: false,
  scope: "local",
  worktree: "",
  effort: "deeper",

  setComposer(value) {
    set({ composer: value });
  },

  setAttachments(attachments) {
    set({ attachments });
  },

  addAttachments(attachments) {
    set((state) => ({ attachments: [...state.attachments, ...attachments] }));
  },

  removeAttachment(index) {
    set((state) => ({ attachments: state.attachments.filter((_, i) => i !== index) }));
  },

  clear() {
    useReferenceStore.getState().clear();
    set({ composer: "", attachments: [] });
  },

  rememberPrompt(value) {
    const prompt = value.trim();
    if (!prompt) return;
    set((state) => ({
      promptHistory: [prompt, ...state.promptHistory.filter((item) => item !== prompt)].slice(
        0,
        50,
      ),
      promptHistoryIndex: null,
      promptHistoryDraft: "",
    }));
  },

  changeComposer(value) {
    if (get().promptHistoryIndex !== null) {
      set({ composer: value, promptHistoryIndex: null, promptHistoryDraft: "" });
    } else {
      set({ composer: value });
    }
  },

  navigateHistory(direction) {
    const { promptHistory, promptHistoryIndex, composer, promptHistoryDraft } = get();
    if (promptHistory.length === 0) return;
    if (direction < 0) {
      if (promptHistoryIndex === null) {
        set({ promptHistoryDraft: composer, composer: promptHistory[0], promptHistoryIndex: 0 });
        return;
      }
      const next = Math.min(promptHistoryIndex + 1, promptHistory.length - 1);
      set({ composer: promptHistory[next], promptHistoryIndex: next });
      return;
    }
    if (promptHistoryIndex === null) return;
    const next = promptHistoryIndex - 1;
    if (next < 0) {
      set({ composer: promptHistoryDraft, promptHistoryDraft: "", promptHistoryIndex: null });
      return;
    }
    set({ composer: promptHistory[next], promptHistoryIndex: next });
  },

  setMode(mode) {
    if (!isComposerMode(mode) || get().mode === mode) return;
    persistComposerMode(mode);
    set({ mode });
  },

  setAgentMode(mode) {
    if (!isAgentMode(mode) || get().agentMode === mode) return;
    set({ agentMode: mode });
  },

  setModel(model) {
    if (get().model === model) return;
    set({ model });
  },

  setFastMode(value) {
    set({ fastMode: Boolean(value) });
  },

  setScope(scope) {
    if (scope !== "local" && scope !== "cloud") return;
    if (get().scope === scope) return;
    set({ scope });
  },

  setWorktree(worktree) {
    if (get().worktree === worktree) return;
    set({ worktree });
  },

  setEffort(effort) {
    if (get().effort === effort) return;
    set({ effort });
  },
}));
