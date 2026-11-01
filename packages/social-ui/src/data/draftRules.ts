// Draft rules the desktop can show before the engine answers. The engine's approval gate is the
// authority; these helpers only let a screen say what it already sees in the text.

/**
 * The first sentence, as the engine reads it: text up to and including the first ".", "!" or "?",
 * or up to the first line break, whichever comes first. The whole text when neither occurs.
 */
export function firstSentence(body: string): string {
  return /^[^\n.!?]*[.!?]?/.exec(body)?.[0] ?? "";
}

/**
 * True when the first sentence contains one of the phrases, compared case-insensitively. Blank
 * phrases never match, so an empty phrase list matches nothing.
 */
export function disclosureFound(body: string, phrases: readonly string[]): boolean {
  const sentence = firstSentence(body).toLowerCase();
  return phrases.some((phrase) => {
    const needle = phrase.trim().toLowerCase();
    return needle !== "" && sentence.includes(needle);
  });
}
