import { describe, expect, it } from "vitest";

import { diagnosticsCardStatusClass, diagnosticsLabel, diagnosticsStatusClass } from "./shared";

describe("status-tone mapping (single source for Diagnostics, Execution, MCP)", () => {
  it("labels pass/warn/block correctly", () => {
    expect(diagnosticsLabel("pass")).toBe("Pass");
    expect(diagnosticsLabel("warn")).toBe("Warn");
    expect(diagnosticsLabel("block")).toBe("Block");
  });

  it("renders warn in amber, never in the command-accent red or success green", () => {
    const pill = diagnosticsStatusClass("warn");
    expect(pill).toMatch(/warning/);
    expect(pill).not.toMatch(/\baccent\b/);
    expect(pill).not.toMatch(/accent-2/);

    const card = diagnosticsCardStatusClass("warn");
    expect(card).toMatch(/warning/);
    expect(card).not.toMatch(/\baccent\b/);
    expect(card).not.toMatch(/accent-2/);
  });

  it("keeps pass green and block red", () => {
    expect(diagnosticsStatusClass("pass")).toMatch(/accent-2/);
    expect(diagnosticsStatusClass("block")).toMatch(/danger/);
    expect(diagnosticsCardStatusClass("pass")).toMatch(/accent-2/);
    expect(diagnosticsCardStatusClass("block")).toMatch(/danger/);
  });
});
