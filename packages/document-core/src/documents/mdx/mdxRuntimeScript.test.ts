import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import mdxRuntimeScript from "./mdxRuntimeScript.js";

describe("mdxRuntimeScript", () => {
  it("is equal to mdxRuntimeScript.txt", () => {
    expect(mdxRuntimeScript).toBe(readFileSync(new URL("./mdxRuntimeScript.txt", import.meta.url), "utf8"));
  });
});
