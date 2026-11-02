import { describe, expect, it } from "vitest";
import { lookupByFirstToken, lookupByPrefix, lookupCommands } from "./commandIndex.js";

describe("terminal command index", () => {
  it("looks up a complete command by its first token", () => {
    expect(lookupCommands("git status")).toEqual(
      expect.arrayContaining([expect.objectContaining({ command: "git status", category: "git" })]),
    );
  });

  it("returns all commands for a first token and supports partial first-token matches", () => {
    expect(lookupByFirstToken("pnpm").length).toBeGreaterThan(1);
    expect(lookupByPrefix("kub")).toEqual(
      expect.arrayContaining([expect.objectContaining({ command: "kubectl get pods" })]),
    );
  });

  it("fails closed for empty or unknown lookups", () => {
    expect(lookupCommands("")).toEqual([]);
    expect(lookupByPrefix("not-a-command")).toEqual([]);
  });
});
