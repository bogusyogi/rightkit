import { useCallback, useEffect, useMemo, useState } from "react";
import {
  isTauriHost,
  searchWorkspaceMentions,
  type CommandInfo,
  type WorkspaceMention,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import { detectComposerMention, type ComposerMentionTrigger } from "../components/app/composerBarHelpers";
import {
  rankComposerCompletions,
  rankMentionCompletions,
  rankSkillCompletions,
  type ComposerCompletion,
  type SkillUsage,
} from "@rightkit/agent-ui-core/lib/composer/completionRanking";

export type ComposerCompletionMode = "off" | "slash-command" | "workspace-mention" | "skill-mention";

export interface ComposerCompletionState {
  mode: ComposerCompletionMode;
  query: string;
  anchor: { start: number; end: number };
}

export interface UseComposerCompletionsOptions {
  value: string;
  commands: readonly CommandInfo[];
  skills: readonly SkillItem[];
  workspaceRoot?: string;
  historyActive?: boolean;
  mentionResults?: readonly WorkspaceMention[];
  searchMentions?: (
    workspaceRoot: string,
    query: string,
    kind: "file" | "symbol",
  ) => Promise<WorkspaceMention[]>;
  skillUsage?: Readonly<Record<string, SkillUsage>>;
  limit?: number;
}

const OFF: ComposerCompletionState = { mode: "off", query: "", anchor: { start: 0, end: 0 } };

function slashState(value: string, historyActive: boolean): ComposerCompletionState {
  if (historyActive || !value.startsWith("/") || /\s/.test(value.slice(1))) return OFF;
  return {
    mode: "slash-command",
    query: value.slice(1),
    anchor: { start: 0, end: value.length },
  };
}

function stateForValue(value: string, historyActive: boolean): ComposerCompletionState {
  const slash = slashState(value, historyActive);
  if (slash.mode !== "off") return slash;
  const trigger = detectComposerMention(value);
  if (!trigger) return OFF;
  return {
    mode: trigger.kind === "skill" ? "skill-mention" : "workspace-mention",
    query: trigger.query,
    anchor: { start: trigger.start, end: trigger.end },
  };
}

function isActiveSkill(skill: SkillItem): boolean {
  return skill.enabled && skill.valid && skill.lifecycle !== "retired" && skill.lifecycle !== "quarantined" && skill.active !== false;
}

export function useComposerCompletions(options: UseComposerCompletionsOptions) {
  const {
    value,
    commands,
    skills,
    workspaceRoot = "",
    historyActive = false,
    mentionResults,
    searchMentions = searchWorkspaceMentions,
    skillUsage,
    limit = 12,
  } = options;
  const state = useMemo(() => stateForValue(value, historyActive), [value, historyActive]);
  const [loadedMentions, setLoadedMentions] = useState<WorkspaceMention[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  const triggerKey = `${state.mode}:${state.anchor.start}:${state.query}`;
  const dismissed = dismissedKey === triggerKey;

  // biome-ignore lint/correctness/useExhaustiveDependencies: each new completion trigger resets active option.
  useEffect(() => {
    setSelectedIndex(0);
  }, [triggerKey]);

  useEffect(() => {
    if (state.mode !== "workspace-mention" || !workspaceRoot.trim() || !isTauriHost()) {
      setLoadedMentions([]);
      return;
    }
    const trigger = detectComposerMention(value);
    if (!trigger || (trigger.kind !== "file" && trigger.kind !== "symbol")) return;
    const mentionKind = trigger.kind;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void searchMentions(workspaceRoot, state.query, mentionKind).then((results) => {
        if (!cancelled) setLoadedMentions(results);
      }).catch(() => {
        if (!cancelled) setLoadedMentions([]);
      });
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [state.mode, state.query, value, workspaceRoot, searchMentions]);

  const items = useMemo<ComposerCompletion[]>(() => {
    if (dismissed || state.mode === "off") return [];
    if (state.mode === "slash-command") {
      return rankComposerCompletions(commands, skills, state.query, {
        limit,
        skillUsage,
        pruneUnusedBuiltins: !state.query.trim(),
      });
    }
    if (state.mode === "skill-mention") {
      return rankSkillCompletions(skills.filter(isActiveSkill), state.query, { limit });
    }
    return rankMentionCompletions(mentionResults ?? loadedMentions, state.query, 18);
  }, [commands, dismissed, limit, loadedMentions, mentionResults, skillUsage, skills, state]);

  const moveSelection = useCallback((delta: number) => {
    setSelectedIndex((current) => items.length ? (current + delta + items.length) % items.length : 0);
  }, [items.length]);
  const close = useCallback(() => setDismissedKey(triggerKey), [triggerKey]);
  const reopen = useCallback(() => setDismissedKey(null), []);

  return {
    state: dismissed ? { ...state, mode: "off" as const } : state,
    items,
    selectedIndex: Math.min(selectedIndex, Math.max(0, items.length - 1)),
    setSelectedIndex,
    moveSelection,
    close,
    reopen,
    hasItems: items.length > 0,
    trigger: detectComposerMention(value) as ComposerMentionTrigger | null,
  };
}

export { stateForValue };
