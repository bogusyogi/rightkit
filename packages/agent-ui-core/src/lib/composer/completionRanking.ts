import type { CommandInfo, WorkspaceMention } from "../daemon";
import type { SkillItem } from "../types";

export type ComposerCompletionKind = "command" | "skill" | "mention";

export interface ComposerCompletion {
  kind: ComposerCompletionKind;
  key: string;
  label: string;
  description: string;
  insertText: string;
  command?: CommandInfo;
  skill?: SkillItem;
  mention?: WorkspaceMention;
  score: number;
}

export interface SkillUsage {
  usage: number;
  origin?: "local" | "bundled" | string;
}

export interface CompletionRankingOptions {
  limit?: number;
  skillUsage?: Readonly<Record<string, SkillUsage>>;
  /** Bare-slash browsing may omit untouched bundled skills; search never does. */
  pruneUnusedBuiltins?: boolean;
}

export function normalizeCompletionText(value: string): string {
  return value.toLowerCase().replace(/^\//, "").trim();
}

/** Fuzzy ranking copied from the Hermes slash-command completion contract. */
export function fuzzyCompletionScore(value: string, query: string): number {
  const haystack = normalizeCompletionText(value);
  const needle = normalizeCompletionText(query);
  if (!needle) return 1;
  if (haystack === needle) return 300;
  if (haystack.startsWith(needle)) return 220 - haystack.length;
  const included = haystack.indexOf(needle);
  if (included >= 0) return 160 - included;

  let score = 0;
  let cursor = 0;
  for (const character of needle) {
    const found = haystack.indexOf(character, cursor);
    if (found < 0) return 0;
    score += found === cursor ? 12 : 4;
    cursor = found + 1;
  }
  return score;
}

function activeSkill(skill: SkillItem): boolean {
  return (
    skill.enabled &&
    skill.valid &&
    skill.lifecycle !== "retired" &&
    skill.lifecycle !== "quarantined" &&
    skill.active !== false
  );
}

function skillScore(skill: SkillItem, query: string): number {
  return Math.max(
    fuzzyCompletionScore(skill.slug, query),
    fuzzyCompletionScore(skill.name, query),
    fuzzyCompletionScore(skill.description, query),
  );
}

function commandScore(command: CommandInfo, query: string): number {
  return Math.max(
    fuzzyCompletionScore(command.name, query),
    fuzzyCompletionScore(command.usage, query),
    fuzzyCompletionScore(command.description, query),
  );
}

export function rankCommandCompletions(
  commands: readonly CommandInfo[],
  query = "",
  limit = 12,
): ComposerCompletion[] {
  return commands
    .map((command, index) => ({ command, index, score: commandScore(command, query) }))
    .filter((row) => !query.trim() || row.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, limit)
    .map(({ command, score }) => ({
      kind: "command" as const,
      key: `command:${command.name}`,
      label: command.name.replace(/-/g, " "),
      description: command.description,
      insertText: `/${command.name} `,
      command,
      score,
    }));
}

export function rankSkillCompletions(
  skills: readonly SkillItem[],
  query = "",
  options: CompletionRankingOptions = {},
): ComposerCompletion[] {
  const limit = options.limit ?? 12;
  const searching = Boolean(query.trim());
  const usage = options.skillUsage ?? {};
  return skills
    .filter(activeSkill)
    .map((skill, index) => {
      const record = usage[skill.slug] ?? usage[`/${skill.slug}`];
      const score = skillScore(skill, query);
      const browseBoost = record?.usage ?? skill.priority ?? 0;
      return { skill, index, score: score + (searching ? 0 : browseBoost), record };
    })
    .filter(({ score, record }) => {
      if (searching) return score > 0;
      if (!options.pruneUnusedBuiltins) return true;
      return record?.origin !== "bundled" || (record.usage ?? 0) > 0;
    })
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, limit)
    .map(({ skill, score }) => ({
      kind: "skill" as const,
      key: `skill:${skill.slug}`,
      label: skill.name || skill.slug,
      description: skill.description || skill.trigger || "Skill",
      insertText: `/${skill.slug} `,
      skill,
      score,
    }));
}

export function rankMentionCompletions(
  mentions: readonly WorkspaceMention[],
  query = "",
  limit = 18,
): ComposerCompletion[] {
  return mentions
    .map((mention, index) => ({
      mention,
      index,
      score: Math.max(
        fuzzyCompletionScore(mention.label, query),
        fuzzyCompletionScore(mention.detail, query),
        fuzzyCompletionScore(mention.insertText, query),
      ),
    }))
    .filter((row) => !query.trim() || row.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, limit)
    .map(({ mention, score }) => ({
      kind: "mention" as const,
      key: `mention:${mention.kind}:${mention.path}:${mention.line ?? 0}:${mention.label}`,
      label: mention.label,
      description: mention.detail,
      insertText: mention.insertText,
      mention,
      score,
    }));
}

/** Build the single list rendered by the composer completion menu. */
export function rankComposerCompletions(
  commands: readonly CommandInfo[],
  skills: readonly SkillItem[],
  query = "",
  options: CompletionRankingOptions = {},
): ComposerCompletion[] {
  const commandItems = rankCommandCompletions(commands, query, options.limit ?? 12);
  const commandNames = new Set(commands.map((command) => normalizeCompletionText(command.name)));
  const skillItems = rankSkillCompletions(skills, query, {
    ...options,
    limit: options.limit ?? 12,
  }).filter((item) => !commandNames.has(normalizeCompletionText(item.skill?.slug ?? "")));
  // Hermes keeps command and skill groups contiguous so a single menu header
  // does not split while results are re-ranked. Preserve that contract here;
  // ranking remains within each group.
  return [...commandItems, ...skillItems].slice(0, options.limit ?? 12);
}
