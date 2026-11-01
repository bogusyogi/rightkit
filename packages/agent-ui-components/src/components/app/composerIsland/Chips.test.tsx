// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./Chips.tsx", import.meta.url), "utf8");

describe("repository search field", () => {
  // Two defects found by driving the picker by hand: opening it and typing lost
  // every keystroke (no focus), and macOS autocorrect rewrote "sellright" to
  // "Sell right", so the search could never match the repo being typed.
  it("autofocuses the search input", () => {
    expect(source).toMatch(/placeholder="Search repositories[\s\S]{0,600}autoFocus/);
  });

  it("disables text correction on repository names", () => {
    expect(source).toMatch(/autoCorrect="off"/);
    expect(source).toMatch(/autoCapitalize="off"/);
    expect(source).toMatch(/spellCheck=\{false\}/);
  });
});
