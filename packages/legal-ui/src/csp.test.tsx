import { readFileSync } from "node:fs";
import { join } from "node:path";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LegalGateProps } from "./LegalGate";

afterEach(() => {
  cleanup();
  document.head.querySelectorAll("style[data-rightkit-legal-gate]").forEach((node) => node.remove());
  document.documentElement.style.removeProperty("--rk-legal-ui-css");
  vi.resetModules();
});

const props = (): LegalGateProps => ({
  theme: { bg: "#F7F3EC", surface: "#FFF", border: "#E4DCDA", text: "#272322", muted: "#686160", accent: "#FF5630", accentContrast: "#FFF", fontDisplay: "Tanker, sans-serif", fontBody: "sans-serif" },
  appName: "CutRight",
  trustLine: "t",
  documents: [{ title: "License", url: "https://example.test/license" }],
  agreementStatement: "I agree.",
  variant: "first-run",
  onAgree: vi.fn(),
  onQuit: vi.fn(),
});

const rules = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "").replace(/:root\s*\{[^}]*\}/, "").replace(/\s+/g, " ").trim();

describe("LegalGate under a strict style-src CSP", () => {
  it("ships the same rules as a stylesheet export as it injects at runtime", async () => {
    const { LegalGate } = await import("./LegalGate");
    render(<LegalGate {...props()} />);
    const injected = document.head.querySelector("style[data-rightkit-legal-gate]")?.textContent ?? "";
    const file = readFileSync(join(process.cwd(), "legal-ui.css"), "utf8");
    expect(rules(injected)).not.toBe("");
    expect(rules(file)).toBe(rules(injected));
  });

  it("injects no <style> element when legal-ui.css is loaded", async () => {
    document.documentElement.style.setProperty("--rk-legal-ui-css", "1");
    const { LegalGate } = await import("./LegalGate");
    render(<LegalGate {...props()} />);
    expect(document.head.querySelector("style[data-rightkit-legal-gate]")).toBeNull();
  });
});
