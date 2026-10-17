import { describe, expect, it } from "vitest";
import { findLiteralMatches } from "../src/index.js";

const slice = (text: string, query: string, options?: Parameters<typeof findLiteralMatches>[2]) =>
  findLiteralMatches(text, query, options).map((m) => text.slice(m.start, m.end));

describe("findLiteralMatches offsets", () => {
  it("keeps offsets in the original text after a length-changing case fold", () => {
    // "İ".toLowerCase() is two code points; the match after it must still land on "b".
    const text = "aİb";
    expect(findLiteralMatches(text, "b")).toEqual([{ start: 2, end: 3 }]);
    expect(findLiteralMatches("İİ one İ One", "one")).toEqual([
      { start: 3, end: 6 },
      { start: 9, end: 12 },
    ]);
  });

  it("reads whole-word neighbours from the original text after such a character", () => {
    const text = "İİİ cat catalog";
    expect(slice(text, "cat", { wholeWord: true })).toEqual(["cat"]);
    expect(findLiteralMatches(text, "cat", { wholeWord: true })).toEqual([{ start: 4, end: 7 }]);
  });

  it("does not match half of a character that folds to two", () => {
    expect(findLiteralMatches("İ", "i")).toEqual([]);
    expect(findLiteralMatches("İ", "İ")).toEqual([{ start: 0, end: 1 }]);
  });

  it("folds Greek capital sigma and final sigma together", () => {
    expect(slice("ΟΔΟΣ x", "οδος")).toEqual(["ΟΔΟΣ"]);
    expect(slice("οδος", "ΟΔΟΣ")).toEqual(["οδος"]);
  });

  it("honours match-case without folding", () => {
    expect(slice("One one", "one", { matchCase: true })).toEqual(["one"]);
    expect(slice("One one", "one")).toEqual(["One", "one"]);
  });

  it("handles characters outside the BMP when checking word boundaries", () => {
    const text = "\u{1d400}cat cat";
    expect(findLiteralMatches(text, "cat", { wholeWord: true })).toEqual([{ start: 6, end: 9 }]);
  });
});

describe("findLiteralMatches whitespace", () => {
  it("finds a one-space phrase in text with a longer run, and covers the whole run", () => {
    const text = "red  \t\n  apple";
    expect(findLiteralMatches(text, "red apple")).toEqual([{ start: 0, end: text.length }]);
  });

  it("collapses whitespace in the query and ignores its ends", () => {
    expect(slice("a b c", "  a \t b ")).toEqual(["a b"]);
    expect(findLiteralMatches("a b", "   ")).toEqual([]);
    expect(findLiteralMatches("a b", "")).toEqual([]);
  });

  it("treats a no-break space as whitespace", () => {
    expect(slice("one two", "one two")).toEqual(["one two"]);
  });

  it("stops a match before the next word", () => {
    expect(findLiteralMatches("one  two", "one")).toEqual([{ start: 0, end: 3 }]);
  });
});

describe("findLiteralMatches optional hyphen joining", () => {
  it("is off by default", () => {
    expect(findLiteralMatches("inter- national", "international")).toEqual([]);
  });

  it("joins an end-of-line hyphen between letters when asked", () => {
    const text = "the inter- national law";
    expect(slice(text, "international", { joinHyphenated: true })).toEqual(["inter- national"]);
    expect(findLiteralMatches(text, "international", { joinHyphenated: true })).toEqual([
      { start: 4, end: 19 },
    ]);
  });

  it("keeps a hyphen that is not followed by whitespace and a letter", () => {
    expect(findLiteralMatches("well-known", "wellknown", { joinHyphenated: true })).toEqual([]);
    expect(findLiteralMatches("end - 5", "end5", { joinHyphenated: true })).toEqual([]);
  });
});

describe("findLiteralMatches special folds", () => {
  it("matches ß only against ß: lower case has no full case fold to SS", () => {
    expect(findLiteralMatches("Straße", "straße")).toEqual([{ start: 0, end: 6 }]);
    expect(findLiteralMatches("STRASSE", "straße")).toEqual([]);
    expect(findLiteralMatches("Straße x", "x")).toEqual([{ start: 7, end: 8 }]);
  });

  it("keeps offsets exact after a ligature, which lower-cases to itself", () => {
    expect(findLiteralMatches("ﬁx", "x")).toEqual([{ start: 1, end: 2 }]);
    expect(findLiteralMatches("ﬁ ﬁx", "ﬁx")).toEqual([{ start: 2, end: 4 }]);
    expect(findLiteralMatches("ﬁ fi", "fi")).toEqual([{ start: 2, end: 4 }]);
  });

  it("collapses whitespace runs around a ß or İ match", () => {
    expect(slice("Straße  İst", "straße İst")).toEqual(["Straße  İst"]);
    expect(slice("İ  ß  x", "İ ß")).toEqual(["İ  ß"]);
  });
});
