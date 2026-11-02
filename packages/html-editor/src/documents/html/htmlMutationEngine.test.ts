// @vitest-environment node
import { describe, expect, it } from "vitest";
import {
  type HtmlMutationPlan,
  type HtmlSourcePatch,
  commitHtmlMutation,
} from "./htmlMutationEngine.js";
import { sourceChecksum } from "./sourceMap.js";

function findRange(source: string, needle: string): { start: number; end: number } {
  const start = source.indexOf(needle);
  if (start < 0) throw new Error(`"${needle}" not found in source`);
  return { start, end: start + needle.length };
}

function makePlan(
  source: string,
  operation: string,
  mutationBoundary: { start: number; end: number },
  impactBoundary: { start: number; end: number },
  patches: readonly HtmlSourcePatch[],
  expectedRevision: string | number = sourceChecksum(source),
): HtmlMutationPlan {
  return { operation, expectedRevision, mutationBoundary, impactBoundary, patches };
}

describe("HTML mutation source-boundary guards", () => {
  it("rejects overlapping patches", () => {
    const source = "<p>abc</p>";
    const boundary = findRange(source, source);
    const plan = makePlan(source, "x", boundary, boundary, [
      { range: findRange(source, "ab"), expectedText: "ab", text: "AB" },
      { range: findRange(source, "bc"), expectedText: "bc", text: "BC" },
    ]);
    expect(() => commitHtmlMutation(source, plan)).toThrowError(
      expect.objectContaining({ code: "patch-overlap" }),
    );
  });

  it("rejects a patch outside the mutation boundary", () => {
    const source = "<p>abc</p>";
    const plan = makePlan(source, "x", findRange(source, "abc"), findRange(source, source), [
      { range: findRange(source, "<p>"), expectedText: "<p>", text: "<div>" },
    ]);
    expect(() => commitHtmlMutation(source, plan)).toThrowError(
      expect.objectContaining({ code: "boundary-violation" }),
    );
  });

  it("rejects a stale exact slice (expectedText guard)", () => {
    const source = "<p>abc</p>";
    const range = findRange(source, "abc");
    const plan = makePlan(source, "replaceText", range, range, [
      { range, expectedText: "xyz", text: "def" },
    ]);
    expect(() => commitHtmlMutation(source, plan)).toThrowError(
      expect.objectContaining({ code: "expected-text-mismatch" }),
    );
  });

  it("rejects a stale revision", () => {
    const source = "<p>abc</p>";
    const plan = makePlan(
      source,
      "replaceText",
      findRange(source, "abc"),
      findRange(source, source),
      [{ range: findRange(source, "abc"), expectedText: "abc", text: "def" }],
      "old-revision",
    );
    expect(() => commitHtmlMutation(source, plan)).toThrowError(
      expect.objectContaining({ code: "stale-revision" }),
    );
  });

  it("a stale inverse fails closed instead of guessing", () => {
    const source = "<p>abc</p>";
    const plan = makePlan(
      source,
      "replaceText",
      findRange(source, "abc"),
      findRange(source, source),
      [{ range: findRange(source, "abc"), expectedText: "abc", text: "def" }],
    );
    const committed = commitHtmlMutation(source, plan);
    const foreign = committed.source.replace("def", "zzz");
    expect(() => commitHtmlMutation(foreign, committed.inversePlan)).toThrowError(
      expect.objectContaining({ code: "stale-revision" }),
    );
  });
});
