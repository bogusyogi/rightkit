// @vitest-environment node
import fs from "node:fs";
import { describe, expect, it } from "vitest";

const sourcePath = new URL("./useSkillsRuntime.ts", import.meta.url);

describe("useSkillsRuntime concurrency guard", () => {
  it("guards skill reloads so stale workspace responses cannot overwrite newer state", () => {
    const source = fs.readFileSync(sourcePath, "utf8");

    expect(source).toContain("skillLoadSeqRef");
    expect(source).toMatch(/const loadSeq = \+\+skillLoadSeqRef\.current/);
    expect(source).toContain("if (loadSeq !== skillLoadSeqRef.current) return");
  });
});
