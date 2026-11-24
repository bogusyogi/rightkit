// Turns a raw error message (an engine, OS or backend string) into the wording shown to the user, by walking a
// rule table in order. The rules and every sentence belong to the host. Imports nothing.

/**
 * One row of the table. The first rule that matches decides the result; later rules are not consulted.
 * Matching is case-insensitive on `includes` / `startsWith`, or by `pattern` against the original message.
 */
export interface ErrorRule {
  /** Matches when the lower-cased message contains any of these (give them lower-case). */
  includes?: readonly string[];
  /** Matches when the lower-cased message starts with any of these (give them lower-case). */
  startsWith?: readonly string[];
  /** Matches when this expression matches the original message. With `strip`, the matched text is cut off the front. */
  pattern?: RegExp;
  /**
   * What to show: `"message"` the raw message unchanged (the message already explains itself), `"strip"` the
   * message without the text `pattern` matched (a typed code prefix), or a string that replaces it.
   */
  show: "message" | "strip" | string;
}

/** Options of {@link humanizeError}. */
export interface HumanizeErrorOptions {
  /** Shown when no rule matches. */
  fallback: string;
}

/**
 * Applies `rules` to `message` in order and returns the wording to show; `options.fallback` when no rule
 * matches. A rule with several conditions matches when any one holds. `show: "strip"` needs a `pattern` and
 * removes the part it matched from the start of the message.
 */
export function humanizeError(message: string, rules: readonly ErrorRule[], options: HumanizeErrorOptions): string {
  const lower = message.toLowerCase();
  for (const rule of rules) {
    let matchedPattern: RegExpExecArray | null = null;
    if (rule.pattern) matchedPattern = rule.pattern.exec(message);
    const hit =
      matchedPattern !== null ||
      (rule.includes?.some((needle) => lower.includes(needle)) ?? false) ||
      (rule.startsWith?.some((prefix) => lower.startsWith(prefix)) ?? false);
    if (!hit) continue;
    if (rule.show === "message") return message;
    if (rule.show === "strip") return matchedPattern ? message.slice(matchedPattern[0].length) : message;
    return rule.show;
  }
  return options.fallback;
}

/** Binds a rule table and fallback once: `const humanize = createErrorHumanizer(rules, { fallback })`. */
export function createErrorHumanizer(
  rules: readonly ErrorRule[],
  options: HumanizeErrorOptions,
): (message: string) => string {
  return (message) => humanizeError(message, rules, options);
}
