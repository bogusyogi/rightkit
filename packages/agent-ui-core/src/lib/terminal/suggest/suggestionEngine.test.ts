import { describe, expect, it } from "vitest";
import { createHistoryProvider, suggest, type ProjectContext } from "./suggestionEngine.js";

const rustContext: ProjectContext = {
  hasGit: true,
  packageManager: "pnpm",
  languages: ["rust", "typescript"],
  frameworks: ["vite"],
};

describe("terminal suggestion engine", () => {
  it("returns no suggestions for an empty input", () => {
    expect(suggest("  ", rustContext, createHistoryProvider())).toEqual([]);
  });

  it("prioritizes a recent history match over the catalog", () => {
    const history = createHistoryProvider(["git status --short", "git log --oneline", "git status --short"]);
    const result = suggest("git status --s", rustContext, history);
    expect(result[0]?.text).toBe("git status --short");
    expect(result[0]?.source).toBe("history");
    expect(result[0]?.score).toBeGreaterThan(300);
  });

  it("applies project relevance, deduplicates, and bounds results", () => {
    const result = suggest("cargo", rustContext, createHistoryProvider());
    expect(result[0]?.text).toBe("cargo add");
    expect(result.every((item) => item.text.startsWith("cargo"))).toBe(true);
    expect(result.length).toBeLessThanOrEqual(15);
  });

  it("preserves descriptions and category badges for catalog entries", () => {
    const result = suggest("pnpm install", rustContext, createHistoryProvider());
    expect(result[0]).toMatchObject({
      text: "pnpm install",
      description: "Install dependencies",
      badge: "pnpm",
    });
  });

  it("trims history prefixes and preserves frequency through recency reordering", () => {
    const history = createHistoryProvider();
    history.addCommand("git status");
    history.addCommand("git log");
    history.addCommand("git status");
    history.addCommand("git diff");

    expect(history.match("  git  ").map((item) => item.command)).toEqual([
      "git diff",
      "git status",
      "git log",
    ]);
    expect(history.match("git status")).toEqual([
      { command: "git status", frequency: 2, recencyIndex: 1 },
    ]);
  });

  it("does not make unrelated project tools relevant", () => {
    const context: ProjectContext = { hasGit: false, packageManager: null, languages: [], frameworks: [] };
    expect(suggest("docker p", context, createHistoryProvider())[0]?.score).toBe(200);
    expect(suggest("docker p", { ...context, frameworks: ["docker"] }, createHistoryProvider())[0]?.score).toBe(350);
  });

  it("caps history at 500 entries and drops the oldest frequency", () => {
    const history = createHistoryProvider();
    history.addCommand("target-for-eviction");
    for (let i = 1; i < 500; i++) history.addCommand(`fill-${i}`);
    expect(history.match("target-for-eviction")).toHaveLength(1);
    history.addCommand("overflow");
    expect(history.match("target-for-eviction")).toEqual([]);
  });
});
