export interface TextMatch {
  start: number;
  end: number;
}

export interface LiteralMatchOptions {
  matchCase?: boolean;
  wholeWord?: boolean;
  /**
   * Join a word broken by a hyphen at the end of a line: a hyphen (`-`, U+2010, U+00AD) between two
   * letters and followed by whitespace is ignored in the text, so `inter- national` matches
   * `international`. Off by default because it also hides a real hyphen followed by a space.
   */
  joinHyphenated?: boolean;
}

const isWordChar = (ch: string | undefined) => !!ch && /[\p{L}\p{N}_]/u.test(ch);
const isSpace = (ch: string) => /\s/u.test(ch);
const isLetter = (ch: string | undefined) => !!ch && /\p{L}/u.test(ch);
const isHyphen = (ch: string) => ch === "-" || ch === "‐" || ch === "­";

/** The code point that ends at UTF-16 index `index` (exclusive), as a string. */
function charBefore(text: string, index: number): string | undefined {
  if (index <= 0) return undefined;
  const low = text.charCodeAt(index - 1);
  if (low >= 0xdc00 && low <= 0xdfff && index >= 2) {
    const high = text.charCodeAt(index - 2);
    if (high >= 0xd800 && high <= 0xdbff) return text.slice(index - 2, index);
  }
  return text[index - 1];
}

/** The code point that starts at UTF-16 index `index`, as a string. */
function charAt(text: string, index: number): string | undefined {
  if (index >= text.length) return undefined;
  const code = text.codePointAt(index);
  return code === undefined ? undefined : String.fromCodePoint(code);
}

/** One character folded to the string it compares as. Per character, never over the whole string,
 *  so the position of every folded unit maps back to the character that made it. */
function foldChar(ch: string, matchCase: boolean): string {
  if (matchCase) return ch;
  // `toLowerCase` is locale independent: the same text matches in every locale. A length-changing
  // fold (U+0130 becomes `i` plus a combining dot) is fine, because each folded unit remembers its
  // source character.
  const lower = ch.toLowerCase();
  // The final sigma only appears in context (`toLowerCase` of one capital sigma is the medial form);
  // fold both lower-case forms to one.
  return lower === "ς" ? "σ" : lower;
}

interface Folded {
  /** The folded, whitespace-collapsed string that is searched. */
  text: string;
  /** For each UTF-16 unit of `text`: where its source character starts in the original string. */
  starts: number[];
  /** For each UTF-16 unit of `text`: where its source character (or whitespace run) ends. */
  ends: number[];
}

function fold(source: string, matchCase: boolean, joinHyphenated: boolean): Folded {
  const chars = Array.from(source);
  const out: string[] = [];
  const starts: number[] = [];
  const ends: number[] = [];
  const push = (value: string, start: number, end: number) => {
    out.push(value);
    for (let i = 0; i < value.length; i += 1) {
      starts.push(start);
      ends.push(end);
    }
  };
  let offset = 0;
  let index = 0;
  let previous: string | undefined;
  while (index < chars.length) {
    const ch = chars[index];
    const start = offset;
    if (isSpace(ch)) {
      // A run of whitespace is one space whose source is the whole run.
      let end = offset + ch.length;
      let next = index + 1;
      while (next < chars.length && isSpace(chars[next])) {
        end += chars[next].length;
        next += 1;
      }
      push(" ", start, end);
      offset = end;
      index = next;
      previous = " ";
      continue;
    }
    if (joinHyphenated && isHyphen(ch) && isLetter(previous)) {
      let next = index + 1;
      let end = offset + ch.length;
      let sawSpace = false;
      while (next < chars.length && isSpace(chars[next])) {
        sawSpace = true;
        end += chars[next].length;
        next += 1;
      }
      if (sawSpace && isLetter(chars[next])) {
        // The hyphen and the line break after it contribute nothing; the next letter joins `previous`.
        offset = end;
        index = next;
        continue;
      }
    }
    push(foldChar(ch, matchCase), start, start + ch.length);
    offset += ch.length;
    index += 1;
    previous = ch;
  }
  return { text: out.join(""), starts, ends };
}

/**
 * Literal (non-regex) find honoring match-case and whole-word. Offsets are UTF-16 offsets into
 * `text` (the string as given), also when a case fold changes a character's length. Runs of
 * whitespace (spaces, tabs, line breaks, no-break spaces) in the text and in `query` each count as
 * one space, so a phrase typed with one space finds text with two; the match then covers the whole
 * run. A match always starts and ends on a character boundary of `text`.
 */
export function findLiteralMatches(
  text: string,
  query: string,
  options?: LiteralMatchOptions,
): TextMatch[] {
  const matchCase = options?.matchCase ?? false;
  const wholeWord = options?.wholeWord ?? false;
  const joinHyphenated = options?.joinHyphenated ?? false;
  // The query is trimmed after folding: leading and trailing whitespace is not part of the phrase.
  const needle = fold(query, matchCase, false).text.trim();
  if (!needle) return [];
  const haystack = fold(text, matchCase, joinHyphenated);
  const matches: TextMatch[] = [];
  let pos = haystack.text.indexOf(needle);
  while (pos !== -1) {
    const last = pos + needle.length - 1;
    const start = haystack.starts[pos];
    const end = haystack.ends[last];
    // A folded unit made by a longer fold belongs to one source character: a match may not cut it.
    const startsOnBoundary = pos === 0 || haystack.starts[pos - 1] !== start;
    const endsOnBoundary =
      last + 1 >= haystack.text.length || haystack.starts[last + 1] !== haystack.starts[last];
    if (startsOnBoundary && endsOnBoundary) {
      // Whole-word boundaries are read from the original text around the match.
      if (!wholeWord || (!isWordChar(charBefore(text, start)) && !isWordChar(charAt(text, end)))) {
        matches.push({ start, end });
      }
      pos = haystack.text.indexOf(needle, pos + needle.length);
    } else {
      pos = haystack.text.indexOf(needle, pos + 1);
    }
  }
  return matches;
}
