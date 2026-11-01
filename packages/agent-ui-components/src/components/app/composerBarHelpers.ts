import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import type { RunTarget } from "../../hooks/useShellUiState";
import type { WorkspaceMention } from "@rightkit/agent-ui-core/lib/daemon";

export function runLabel(target: RunTarget) {
  switch (target) {
    case "worktree":
      return "Worktree";
    default:
      return "Local";
  }
}

export function goalStatusLabel(status: string | null) {
  switch (status) {
    case "paused":
      return "Goal paused";
    case "blocked":
      return "Goal blocked";
    case "budget-limited":
      return "Goal budget";
    case "usage-limited":
      return "Goal usage";
    case "achieved":
      return "Goal achieved";
    case "unmet":
      return "Goal unmet";
    default:
      return "Goal active";
  }
}

export function skillStatusLabel(skill: SkillItem) {
  if (!skill.valid) return "Invalid";
  if (skill.lifecycle === "quarantined") return "Quarantined";
  if (skill.lifecycle === "retired") return "Retired";
  if (skill.active === false || !skill.enabled) return "Disabled";
  return "Active";
}

export function skillTriggerLabel(skill: SkillItem) {
  if (skill.disableModelInvocation) return "User-only";
  return skill.trigger?.trim() || "Model-invoked";
}

export function skillScopeLabel(skill: SkillItem) {
  const parts = [
    skill.category,
    skill.kind,
    skill.effects?.executionMode ? `mode ${skill.effects.executionMode}` : null,
  ].filter(Boolean);
  return parts.join(" · ");
}

export function skillToolLabel(skill: SkillItem) {
  const allowed = skill.effects?.allowedTools ?? [];
  const denied = skill.effects?.disallowedTools ?? [];
  if (allowed.length > 0) return `Allows ${allowed.slice(0, 3).join(", ")}`;
  if (denied.length > 0) return `Blocks ${denied.slice(0, 3).join(", ")}`;
  return skill.metadata?.coderight?.requiresToolsets?.length
    ? `Needs ${skill.metadata.coderight.requiresToolsets.slice(0, 3).join(", ")}`
    : "No tool constraints";
}

export type ComposerMentionKind = "file" | "symbol" | "skill";

export interface ComposerMentionTrigger {
  kind: ComposerMentionKind;
  query: string;
  start: number;
  end: number;
  key: string;
}

function mentionQuery(value: string): string {
  const query = value.trim();
  // Hermes' path references allow quoted paths, and the the host app drop path
  // formatter uses backticks when a path contains whitespace. Keep the token
  // bounds intact for replacement, but do not send the quote delimiter to the
  // daemon search endpoint.
  if (query.length >= 2) {
    const first = query[0];
    const last = query.at(-1);
    if ((first === "`" && last === "`") || (first === '"' && last === '"')) {
      return query.slice(1, -1);
    }
  }
  return query.replace(/^[`"]/, "");
}

function triggerFromMatch(
  value: string,
  match: RegExpExecArray,
  kind: ComposerMentionKind,
  rawQuery: string,
): ComposerMentionTrigger {
  const leading = match[1] ?? "";
  const tokenStart = match.index + leading.length;
  const tokenEnd = value.length;
  const query = mentionQuery(rawQuery);
  return {
    kind,
    query,
    start: tokenStart,
    end: tokenEnd,
    key: `${tokenStart}:${kind}:${query}`,
  };
}

function lastMentionStarter(
  value: string,
  pattern: RegExp,
): RegExpExecArray | null {
  let last: RegExpExecArray | null = null;
  let match = pattern.exec(value);
  while (match !== null) {
    last = match;
    // Both patterns consume at least the starter, but keep this guard so the
    // helper remains safe if a future caller supplies an empty-match pattern.
    if (match[0].length === 0) pattern.lastIndex += 1;
    match = pattern.exec(value);
  }
  return last;
}

export function detectComposerMention(value: string): ComposerMentionTrigger | null {
  // Find the last starter, then capture the rest of its current line rather
  // than only [^\\s]*. That keeps completion usable for quoted paths such as
  // @file:`src/feature flags.ts` while ensuring a prior mention does not own
  // the query after the user starts a new one.
  const slashSkill = lastMentionStarter(value, /(^|\s)\/skill(?::|\s)?/gi);
  const mention = lastMentionStarter(value, /(^|\s)@(file|symbol|skill)(?::|\s)?/gi);
  if (!slashSkill && !mention) return null;

  const active = !mention || (slashSkill && slashSkill.index > mention.index) ? slashSkill : mention;
  if (!active || active.index === undefined) return null;
  const queryStart = active.index + active[0].length;
  // A completion belongs to the line containing the caret. A trigger from a
  // previous line must not consume the current line as its path query.
  if (value.slice(queryStart).includes("\n")) return null;
  const kind = active === slashSkill ? "skill" : (active[2].toLowerCase() as ComposerMentionKind);
  return triggerFromMatch(value, active, kind, value.slice(queryStart));
}

export type ComposerMention = WorkspaceMention;
