/**
 * Palette / Jump-to ranking (moved from @rightkit/react-shell 0.6.0, behaviour unchanged). Merges ViewRight's
 * subsequence scorer (typo-tolerant, rewards early hits) with HeardRight/CodeRight's substring + keyword matching.
 * Higher score is better; null is no match.
 */

function subsequence(query: string, text: string): number | null {
  let qi = 0;
  let firstHit = -1;
  let gaps = 0;
  for (let li = 0; li < text.length && qi < query.length; li++) {
    if (query[qi] === text[li]) {
      if (firstHit < 0) firstHit = li;
      qi++;
    } else if (firstHit >= 0) {
      gaps++;
    }
  }
  return qi === query.length ? 40 - Math.min(30, firstHit + gaps) : null;
}

export function matchScore(query: string, label: string, keywords: readonly string[] = [], hint = ""): number | null {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const l = label.toLowerCase();
  if (l === q) return 200;
  if (l.startsWith(q)) return 160 - Math.min(20, l.length - q.length);
  const word = l.split(/[\s/_.:›-]+/).findIndex((w) => w.startsWith(q));
  if (word >= 0) return 120 - word;
  const at = l.indexOf(q);
  if (at >= 0) return 100 - Math.min(40, at);
  if (keywords.some((k) => k.toLowerCase().includes(q))) return 70;
  if (hint && hint.toLowerCase().includes(q)) return 60;
  return subsequence(q, l);
}

export interface Rankable {
  label: string;
  keywords?: readonly string[];
  hint?: string;
  /** Sub-label, matched only with `RankOptions.subLabel` (opt-in donor parity). */
  sub?: string;
  /** Exact/prefix aliases, matched only with `RankOptions.aliasTiers` (opt-in donor parity). */
  aliases?: readonly string[];
}

/** Opt-in donor-parity ranking extras (CodeRight paletteSearch). Default ranking is unchanged. */
export interface RankOptions {
  /** Match `Rankable.sub` (prefix above label substring; substring below it). */
  subLabel?: boolean;
  /** Match `Rankable.aliases` with exact (below label prefix) and prefix tiers. */
  aliasTiers?: boolean;
  /** `"input"` (default): ties keep input order. `"label"`: ties sort by label, then input order. */
  tieBreak?: "input" | "label";
}

function extendedScore(query: string, item: Rankable, options: RankOptions): number | null {
  const base = matchScore(query, item.label, item.keywords, item.hint);
  const q = query.trim().toLowerCase();
  if (!q) return base;
  let best = base;
  const raise = (score: number) => {
    if (best === null || score > best) best = score;
  };
  if (options.aliasTiers && item.aliases) {
    for (const alias of item.aliases) {
      const a = alias.toLowerCase();
      if (a === q) raise(135);
      else if (a.startsWith(q)) raise(125);
    }
  }
  if (options.subLabel && item.sub) {
    const s = item.sub.toLowerCase();
    if (s.startsWith(q)) raise(110);
    else {
      const at = s.indexOf(q);
      if (at >= 0) raise(90 - Math.min(20, at));
    }
  }
  return best;
}

/** Stable sort by score; ties keep input order unless `options.tieBreak` opts into label order. Empty query keeps everything in input order. */
export function rankItems<T extends Rankable>(items: readonly T[], query: string, limit = 50, options: RankOptions = {}): T[] {
  const scored: { item: T; score: number; index: number }[] = [];
  items.forEach((item, index) => {
    const score = options.subLabel || options.aliasTiers ? extendedScore(query, item, options) : matchScore(query, item.label, item.keywords, item.hint);
    if (score !== null) scored.push({ item, score, index });
  });
  if (query.trim()) {
    scored.sort((a, b) =>
      b.score - a.score || (options.tieBreak === "label" ? a.item.label.localeCompare(b.item.label) || a.index - b.index : a.index - b.index),
    );
  }
  return scored.slice(0, limit).map((entry) => entry.item);
}

/** Built-in command ranking: substring, keyword, hint and subsequence matching; empty query keeps input order. */
export function rankCommands<T extends Rankable>(commands: readonly T[], query: string, limit = 50, options: RankOptions = {}): T[] {
  return rankItems(commands, query, limit, options);
}
