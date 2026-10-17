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

export interface RegexMatchOptions {
  matchCase?: boolean;
  wholeWord?: boolean;
}

/** Longest regex source accepted from a user. */
export const MAX_REGEX_PATTERN_LENGTH = 200;
/** Most unbounded repeats (`*`, `+`, `{n,}`) a pattern may contain under the strict (synchronous) policy. */
export const MAX_REGEX_REPEATS = 1;
/** Largest bounded-repeat upper bound (`{n}`, `{n,m}`) the strict policy accepts. */
export const MAX_REGEX_BOUND = 1000;
/** Default per-page deadline for `findRegexMatchesBounded` before the worker is terminated. */
export const REGEX_DEADLINE_MS = 250;
/** Text longer than this is scanned only up to this many UTF-16 units. */
export const MAX_REGEX_TEXT = 100_000;

interface GroupFrame {
  /** A repeat operator sits inside this group (or a group nested in it). */
  quantified: boolean;
  alternation: boolean;
}

/** Length of the repeat operator at `index` (`*`, `+`, `?`, `{n}`, `{n,}`, `{n,m}`), or 0. */
function repeatLength(chars: string[], index: number): number {
  const ch = chars[index];
  if (ch === "*" || ch === "+" || ch === "?") return 1;
  if (ch === "{") {
    const match = /^\{\d+(?:,\d*)?\}/.exec(chars.slice(index, index + 24).join(""));
    return match ? match[0].length : 0;
  }
  return 0;
}

/**
 * Checks a user-supplied regex source before it is compiled. Returns a reason it is refused, or
 * null when it is accepted. Refused: sources longer than `MAX_REGEX_PATTERN_LENGTH`, backreferences,
 * and invalid syntax. The "strict" policy (default; used wherever matching runs synchronously on the
 * calling thread) also refuses a repeat applied to a group that repeats or contains alternation at any
 * depth (`(a+)+`, `(?:(a|aa))+`), more than `MAX_REGEX_REPEATS` unbounded repeat (`a*a*b`), and
 * repeat bounds above `MAX_REGEX_BOUND`. JavaScript has no step budget, so strict is a structural
 * mitigation only; unbounded execution is prevented by `findRegexMatchesBounded`, which runs the
 * "basic" policy inside a terminable Worker. The pattern is only ever compiled with `RegExp`.
 */
export function regexPatternError(pattern: string, policy: "strict" | "basic" = "strict"): string | null {
  const strict = policy === "strict";
  if (pattern.length > MAX_REGEX_PATTERN_LENGTH) return `pattern is longer than ${MAX_REGEX_PATTERN_LENGTH} characters`;
  const chars = Array.from(pattern);
  const stack: GroupFrame[] = [{ quantified: false, alternation: false }];
  let repeats = 0;
  let afterRepeat = false;
  let i = 0;
  while (i < chars.length) {
    const ch = chars[i];
    if (ch === "\\") {
      const next = chars[i + 1];
      if (next !== undefined && (/[1-9k]/.test(next))) return "backreferences are not allowed";
      i += 2;
      afterRepeat = false;
      continue;
    }
    if (ch === "[") {
      i += 1;
      if (chars[i] === "^") i += 1;
      while (i < chars.length && chars[i] !== "]") i += chars[i] === "\\" ? 2 : 1;
      i += 1;
      afterRepeat = false;
      continue;
    }
    if (ch === "(") {
      stack.push({ quantified: false, alternation: false });
      i += 1;
      if (chars[i] === "?") {
        i += 1;
        if (chars[i] === "<" && chars[i + 1] !== "=" && chars[i + 1] !== "!") {
          const close = chars.indexOf(">", i);
          if (close < 0) break;
          i = close + 1;
        } else if (chars[i] === "<") {
          i += 2;
        } else {
          i += 1;
        }
      }
      afterRepeat = false;
      continue;
    }
    if (ch === ")") {
      if (stack.length === 1) break;
      const frame = stack.pop() as GroupFrame;
      i += 1;
      const next = chars[i];
      if (strict && (next === "*" || next === "+" || next === "{") && repeatLength(chars, i) > 0 && (frame.quantified || frame.alternation)) {
        return "a repeat on a group that repeats or alternates is not allowed";
      }
      const parent = stack[stack.length - 1];
      parent.quantified ||= frame.quantified;
      parent.alternation ||= frame.alternation;
      afterRepeat = false;
      continue;
    }
    if (ch === "|") {
      stack[stack.length - 1].alternation = true;
      i += 1;
      afterRepeat = false;
      continue;
    }
    const length = repeatLength(chars, i);
    if (length > 0) {
      if (afterRepeat && ch === "?") {
        // Lazy modifier of the repeat just read.
        i += 1;
        afterRepeat = false;
        continue;
      }
      const token = chars.slice(i, i + length).join("");
      if (ch === "*" || ch === "+" || /^\{\d+,\}$/.test(token)) repeats += 1;
      stack[stack.length - 1].quantified = true;
      if (strict && repeats > MAX_REGEX_REPEATS) return `more than ${MAX_REGEX_REPEATS} unbounded repeat is not allowed`;
      if (strict && ch === "{") {
        const nums = token.match(/\d+/g) ?? [];
        if (nums.some((n) => Number(n) > MAX_REGEX_BOUND)) return `repeat bounds above ${MAX_REGEX_BOUND} are not allowed`;
      }
      i += length;
      afterRepeat = true;
      continue;
    }
    afterRepeat = false;
    i += 1;
  }
  try {
    new RegExp(pattern, "u");
  } catch {
    return "invalid regular expression";
  }
  return null;
}

/**
 * Regex find over `text`, with the same `matchCase` and `wholeWord` meaning as `findLiteralMatches`.
 * Offsets index `text` directly (no folding, so no drift). A pattern refused by `regexPatternError`,
 * or one that does not compile, matches nothing. Zero-length matches are skipped. Only the first
 * `MAX_REGEX_TEXT` units of `text` are searched.
 */
export function findRegexMatches(text: string, pattern: string, options?: RegexMatchOptions): TextMatch[] {
  if (!pattern || regexPatternError(pattern, "strict") !== null) return [];
  const wholeWord = options?.wholeWord ?? false;
  const haystack = text.length > MAX_REGEX_TEXT ? text.slice(0, MAX_REGEX_TEXT) : text;
  let re: RegExp;
  try {
    re = new RegExp(pattern, options?.matchCase ? "gu" : "giu");
  } catch {
    return [];
  }
  const matches: TextMatch[] = [];
  let found: RegExpExecArray | null = re.exec(haystack);
  while (found !== null) {
    const start = found.index;
    const end = start + found[0].length;
    if (end === start) {
      const code = haystack.codePointAt(start);
      re.lastIndex = start + (code !== undefined && code > 0xffff ? 2 : 1);
    } else if (!wholeWord || (!isWordChar(charBefore(haystack, start)) && !isWordChar(charAt(haystack, end)))) {
      matches.push({ start, end });
    }
    found = re.exec(haystack);
  }
  return matches;
}

export interface BoundedRegexResult {
  matches: TextMatch[];
  /** The deadline passed; `matches` holds only what was found before the worker was terminated. */
  timedOut: boolean;
}

export interface RegexSearcher {
  search(text: string, pattern: string, options?: RegexMatchOptions): Promise<BoundedRegexResult>;
  dispose(): void;
}

// Self-contained worker source (a string, so no bundler or transpiler helper can leak into it).
const REGEX_WORKER_SOURCE = `
const isWord = (ch) => !!ch && /[\\p{L}\\p{N}_]/u.test(ch);
const before = (t, i) => { if (i <= 0) return undefined; const lo = t.charCodeAt(i - 1);
  if (lo >= 0xdc00 && lo <= 0xdfff && i >= 2) { const hi = t.charCodeAt(i - 2); if (hi >= 0xd800 && hi <= 0xdbff) return t.slice(i - 2, i); }
  return t.charAt(i - 1); };
const at = (t, i) => { if (i >= t.length) return undefined; const c = t.codePointAt(i); return c === undefined ? undefined : String.fromCodePoint(c); };
self.onmessage = (event) => {
  const { id, text, pattern, flags, wholeWord } = event.data;
  let re;
  try { re = new RegExp(pattern, flags); } catch (e) { self.postMessage({ id, done: true, matches: [] }); return; }
  let batch = [];
  let found = re.exec(text);
  while (found !== null) {
    const start = found.index; const end = start + found[0].length;
    if (end === start) { const c = text.codePointAt(start); re.lastIndex = start + (c !== undefined && c > 0xffff ? 2 : 1); }
    else if (!wholeWord || (!isWord(before(text, start)) && !isWord(at(text, end)))) {
      batch.push({ start, end });
      if (batch.length >= 256) { self.postMessage({ id, done: false, matches: batch }); batch = []; }
    }
    found = re.exec(text);
  }
  self.postMessage({ id, done: true, matches: batch });
};
`;

/**
 * Regex search that can never stall the calling thread. When `Worker` exists, each `search` runs in
 * a Worker (blob URL, no bundler dependency) under the "basic" policy with a hard `deadlineMs`; on
 * expiry the Worker is terminated (a fresh one starts on the next call) and the result carries
 * `timedOut: true` with the matches posted so far. Without `Worker` (node), it runs `findRegexMatches`
 * synchronously under the strict structural policy. Only the first `MAX_REGEX_TEXT` units are searched.
 */
export function createRegexSearcher(deadlineMs: number = REGEX_DEADLINE_MS): RegexSearcher {
  const canWorker = typeof Worker !== "undefined" && typeof Blob !== "undefined" && typeof URL !== "undefined" && typeof URL.createObjectURL === "function";
  let worker: Worker | null = null;
  let url: string | null = null;
  let nextId = 0;
  const stop = () => {
    worker?.terminate();
    worker = null;
    if (url) { URL.revokeObjectURL(url); url = null; }
  };
  return {
    dispose: stop,
    search(text, pattern, options) {
      if (!pattern) return Promise.resolve({ matches: [], timedOut: false });
      if (!canWorker) return Promise.resolve({ matches: findRegexMatches(text, pattern, options), timedOut: false });
      if (regexPatternError(pattern, "basic") !== null) return Promise.resolve({ matches: [], timedOut: false });
      const haystack = text.length > MAX_REGEX_TEXT ? text.slice(0, MAX_REGEX_TEXT) : text;
      let active: Worker;
      try {
        if (!worker) {
          url = URL.createObjectURL(new Blob([REGEX_WORKER_SOURCE], { type: "text/javascript" }));
          worker = new Worker(url);
        }
        active = worker;
      } catch {
        stop();
        // Worker construction refused (e.g. CSP): fall back to the strict synchronous policy.
        return Promise.resolve({ matches: findRegexMatches(text, pattern, options), timedOut: false });
      }
      const id = ++nextId;
      const matches: TextMatch[] = [];
      return new Promise<BoundedRegexResult>((resolve) => {
        const finish = (timedOut: boolean) => {
          clearTimeout(timer);
          active.onmessage = null;
          active.onerror = null;
          if (timedOut) stop();
          resolve({ matches, timedOut });
        };
        const timer = setTimeout(() => finish(true), deadlineMs);
        active.onmessage = (event: MessageEvent) => {
          const data = event.data as { id: number; done: boolean; matches: TextMatch[] };
          if (data.id !== id) return;
          matches.push(...data.matches);
          if (data.done) finish(false);
        };
        active.onerror = () => finish(true);
        active.postMessage({ id, text: haystack, pattern, flags: options?.matchCase ? "gu" : "giu", wholeWord: options?.wholeWord ?? false });
      });
    },
  };
}
