import { lookupCommands, lookupByPrefix, type CommandEntry } from "./commandIndex.js";

export interface ProjectContext {
  hasGit: boolean;
  packageManager: "npm" | "yarn" | "pnpm" | "bun" | null;
  languages: string[];
  frameworks: string[];
}

export interface HistoryMatch {
  command: string;
  frequency: number;
  recencyIndex: number;
}

/** Minimal history seam carried from Hermes IDE's HistoryProvider. */
export interface HistoryProvider {
  readonly loaded: boolean;
  markLoaded(): void;
  match(prefix: string): HistoryMatch[];
  addCommand(command: string): void;
}

export function createHistoryProvider(commands: string[] = []): HistoryProvider {
  const recency: string[] = [];
  const frequencies = new Map<string, number>();
  // Callers provide newest-first history, while addCommand rebuilds the
  // recency list in arrival order. Replay oldest-first to preserve that order.
  for (const command of [...commands].reverse()) addCommand(command);

  function addCommand(command: string): void {
    const trimmed = command.trim();
    if (!trimmed) return;
    frequencies.set(trimmed, (frequencies.get(trimmed) ?? 0) + 1);
    const existing = recency.indexOf(trimmed);
    if (existing >= 0) recency.splice(existing, 1);
    recency.unshift(trimmed);
    if (recency.length > 500) {
      const removed = recency.pop();
      if (removed) frequencies.delete(removed);
    }
  }

  let loaded = false;
  return {
    get loaded() {
      return loaded;
    },
    markLoaded() {
      loaded = true;
    },
    addCommand,
    match(prefix: string): HistoryMatch[] {
      const trimmed = prefix.trim();
      if (!trimmed) return [];
      return recency.flatMap((command, recencyIndex) =>
        command.startsWith(trimmed)
          ? [{ command, frequency: frequencies.get(command) ?? 1, recencyIndex }]
          : [],
      );
    },
  };
}

export interface Suggestion {
  text: string;
  description?: string;
  source: "history" | "index" | "context";
  score: number;
  badge?: string;
}

const MAX_RESULTS = 15;
const LENGTH_PENALTY_THRESHOLD = 60;

/**
 * Compute suggestions synchronously. Target: <5ms.
 *
 * This is the Hermes IDE scoring contract: history outranks the curated
 * catalog, context adds relevance, identical candidates merge, long commands
 * are penalized, and the result is bounded for a responsive terminal.
 */
export function suggest(
  input: string,
  context: ProjectContext | null,
  history: HistoryProvider,
): Suggestion[] {
  const trimmed = input.trim();
  if (!trimmed) return [];

  const candidates = new Map<string, Suggestion>();

  for (const hm of history.match(trimmed)) {
    const recencyBoost = Math.max(0, 100 - hm.recencyIndex * 2);
    const frequencyBoost = Math.min(hm.frequency * 10, 200);
    addCandidate(candidates, {
      text: hm.command,
      source: "history",
      score: 200 + frequencyBoost + recencyBoost,
    });
  }

  const tokens = trimmed.split(/\s+/);
  const indexMatches: CommandEntry[] = trimmed.includes(" ")
    ? lookupCommands(trimmed)
    : lookupByPrefix(tokens[0]);

  for (const entry of indexMatches) {
    let score = 100;
    if (context && isContextRelevant(entry.category, context)) score += 150;
    if (entry.command.startsWith(trimmed)) score += 100;
    addCandidate(candidates, {
      text: entry.command,
      description: entry.description,
      source: "index",
      score,
      badge: entry.category,
    });
  }

  const results = [...candidates.values()];
  for (const suggestion of results) {
    if (suggestion.text.length > LENGTH_PENALTY_THRESHOLD) {
      suggestion.score -= (suggestion.text.length - LENGTH_PENALTY_THRESHOLD) * 2;
    }
  }
  results.sort((a, b) => b.score - a.score || a.text.localeCompare(b.text));
  return results.slice(0, MAX_RESULTS);
}

export function isContextRelevant(category: string, context: ProjectContext): boolean {
  if (category === "system" || category === "brew" || category === "gh") return true;
  if (category === "git" && context.hasGit) return true;
  if (category === "npm" && context.packageManager === "npm") return true;
  if (category === "yarn" && context.packageManager === "yarn") return true;
  if (category === "pnpm" && context.packageManager === "pnpm") return true;
  if (category === "bun" && context.packageManager === "bun") return true;
  if (category === "cargo" && context.languages.includes("rust")) return true;
  if (category === "python" && context.languages.includes("python")) return true;
  if (category === "go" && context.languages.includes("go")) return true;
  if (category === "typescript" && context.languages.includes("typescript")) return true;
  if (context.frameworks.includes(category)) return true;
  if (category === "docker" && context.frameworks.includes("docker")) return true;
  if (category === "k8s" && context.frameworks.includes("k8s")) return true;
  if (category === "make" && context.frameworks.includes("make")) return true;
  return false;
}

function addCandidate(map: Map<string, Suggestion>, candidate: Suggestion): void {
  const existing = map.get(candidate.text);
  if (!existing) {
    map.set(candidate.text, candidate);
    return;
  }

  const kept = candidate.score > existing.score ? candidate : existing;
  const other = kept === candidate ? existing : candidate;
  kept.score += 50;
  if (!kept.description && other.description) kept.description = other.description;
  if (!kept.badge && other.badge) kept.badge = other.badge;
  map.set(candidate.text, kept);
}
