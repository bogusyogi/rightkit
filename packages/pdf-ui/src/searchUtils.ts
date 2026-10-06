export interface TextMatch {
  start: number;
  end: number;
}

const isWordChar = (ch: string | undefined) => !!ch && /[\p{L}\p{N}_]/u.test(ch);

/** Literal (non-regex) find honoring match-case and whole-word. */
export function findLiteralMatches(
  text: string,
  query: string,
  options?: { matchCase?: boolean; wholeWord?: boolean },
): TextMatch[] {
  if (!query) return [];
  const matchCase = options?.matchCase ?? false;
  const wholeWord = options?.wholeWord ?? false;
  const matches: TextMatch[] = [];
  const haystack = matchCase ? text : text.toLocaleLowerCase();
  const needle = matchCase ? query : query.toLocaleLowerCase();
  let pos = haystack.indexOf(needle);
  while (pos !== -1) {
    const end = pos + query.length;
    // Whole-word boundaries are checked against the original text so case folding can't shift them.
    if (!wholeWord || (!isWordChar(text[pos - 1]) && !isWordChar(text[end]))) {
      matches.push({ start: pos, end });
    }
    pos = haystack.indexOf(needle, pos + needle.length);
  }
  return matches;
}
