import { describe, expect, it } from "vitest";
import { createRegexSearcher, findLiteralMatches, findRegexMatches, regexPatternError, MAX_REGEX_PATTERN_LENGTH } from "../src/index.js";

describe("findRegexMatches", () => {
  it("is case-insensitive by default and exact with matchCase", () => {
    expect(findRegexMatches("Cat cat", "cat")).toEqual([{ start: 0, end: 3 }, { start: 4, end: 7 }]);
    expect(findRegexMatches("Cat cat", "cat", { matchCase: true })).toEqual([{ start: 4, end: 7 }]);
  });

  it("finds a pattern and reports offsets in the original text", () => {
    const text = "İ invoice 12-345 and 67-890";
    expect(findRegexMatches(text, "\\d{2}-\\d{3}")).toEqual([{ start: 10, end: 16 }, { start: 21, end: 27 }]);
  });

  it("honours wholeWord with the same boundaries as the literal find", () => {
    expect(findRegexMatches("cat catalog", "cat", { wholeWord: true })).toEqual([{ start: 0, end: 3 }]);
    expect(findRegexMatches("cat catalog", "cat", { wholeWord: false })).toHaveLength(2);
  });

  it("skips zero-length matches and matches nothing for an empty pattern", () => {
    expect(findRegexMatches("abc", "x*")).toEqual([]);
    expect(findRegexMatches("abc", "")).toEqual([]);
  });

  it("matches nothing for a pattern that does not compile", () => {
    expect(findRegexMatches("a(b", "(")).toEqual([]);
  });

  it("supports lookbehind and named groups", () => {
    expect(findRegexMatches("xa ya", "(?<=y)a")).toEqual([{ start: 4, end: 5 }]);
    expect(findRegexMatches("abab", "(?<pair>ab)+")).toEqual([{ start: 0, end: 4 }]);
  });
});

describe("regexPatternError", () => {
  it("accepts ordinary patterns", () => {
    expect(regexPatternError("\\d{3}-\\d{4}")).toBeNull();
    expect(regexPatternError("a+b")).toBeNull();
    expect(regexPatternError("a{1,1000}")).toBeNull();
    expect(regexPatternError("(a+)?")).toBeNull();
    expect(regexPatternError("[a+]+")).toBeNull();
    expect(regexPatternError("\\+\\+")).toBeNull();
  });

  it("refuses nested or alternating repeats that backtrack badly", () => {
    expect(regexPatternError("(a+)+")).not.toBeNull();
    expect(regexPatternError("(.*)*")).not.toBeNull();
    expect(regexPatternError("(a|a)+")).not.toBeNull();
    expect(regexPatternError("((a)+b)*")).not.toBeNull();
  });

  it("refuses backreferences", () => {
    expect(regexPatternError("(a)\\1")).not.toBeNull();
    expect(regexPatternError("(?<n>a)\\k<n>")).not.toBeNull();
  });

  it("refuses too many unbounded repeats and over-long sources", () => {
    expect(regexPatternError("a*b*c*")).not.toBeNull();
    expect(regexPatternError("a".repeat(MAX_REGEX_PATTERN_LENGTH + 1))).not.toBeNull();
  });

  it("refuses invalid syntax", () => {
    expect(regexPatternError("(")).not.toBeNull();
    expect(regexPatternError("a{")).not.toBeNull();
  });

  it("treats a refused pattern as no match, never as code", () => {
    expect(findRegexMatches("aaaa", "(a+)+")).toEqual([]);
    expect(findLiteralMatches("aaaa", "(a+)+")).toEqual([]);
  });
});

describe("catastrophic patterns (synchronous fallback policy)", () => {
  const astra = ["(?:(a|aa))+b", "a*a*b", "a{0,100000}a{0,100000}b"];
  it("rejects the known catastrophic patterns", () => {
    for (const pattern of astra) {
      expect(regexPatternError(pattern)).not.toBeNull();
      const started = Date.now();
      expect(findRegexMatches("a".repeat(20_000), pattern)).toEqual([]);
      expect(Date.now() - started).toBeLessThan(500);
    }
  });
  it("rejects alternation under a quantified group at any depth and bounds above 1000", () => {
    expect(regexPatternError("((a|b))+")).not.toBeNull();
    expect(regexPatternError("a{1001}")).not.toBeNull();
    expect(regexPatternError("a{0,100000}")).not.toBeNull();
  });
  it("still matches a benign pattern", async () => {
    expect(findRegexMatches("foo123 bar45", "\\d+")).toEqual([{ start: 3, end: 6 }, { start: 10, end: 12 }]);
    const searcher = createRegexSearcher();
    // Under node (no Worker) the searcher takes the synchronous strict path; the Worker path with
    // its terminate-on-deadline behaviour is browser-only and exercised in test/browser-e2e.mjs.
    expect(await searcher.search("foo123", "\\d+")).toEqual({ matches: [{ start: 3, end: 6 }], timedOut: false });
    expect(await searcher.search("aaaa", "a*a*b")).toEqual({ matches: [], timedOut: false });
    searcher.dispose();
  });
});
