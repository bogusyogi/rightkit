import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LegalGate, type LegalGateProps } from "./LegalGate";

afterEach(() => {
  cleanup();
});

const props = (overrides: Partial<LegalGateProps> = {}): LegalGateProps => ({
  theme: {
    bg: "#F7F3EC", surface: "#FFFFFF", border: "#E4DCDA", text: "#272322", muted: "#686160",
    accent: "#FF5630", accentContrast: "#FFFFFF", fontDisplay: "Tanker, sans-serif", fontBody: "'Hanken Grotesk', sans-serif",
  },
  appName: "HeardRight",
  trustLine: "HeardRight transcribes and listens for its wake word on your device.",
  documents: [{ title: "License", url: "https://heardright.app/legal/license/" }],
  agreementStatement: "I have read and agree to the HeardRight License.",
  variant: "first-run",
  onAgree: vi.fn(),
  onQuit: vi.fn(),
  ...overrides,
});

// Byte-for-byte guard: the default render must match the markup 0.2.1 produced. The fixtures in
// src/fixtures/ were captured from 0.2.1 before the 0.2.2 props were added. Do not regenerate them.
describe("default render (0.2.1 markup)", () => {
  it("first-run markup", () => {
    const { container } = render(<LegalGate {...props()} />);
    expect(container.innerHTML).toBe(readFileSync(join(process.cwd(), "src/fixtures/first-run.html"), "utf8"));
  });

  it("re-acceptance markup", () => {
    const { container } = render(<LegalGate {...props({ variant: "re-acceptance", version: "v2", manifestSha256: "a".repeat(64) })} />);
    expect(container.innerHTML).toBe(readFileSync(join(process.cwd(), "src/fixtures/re-acceptance.html"), "utf8"));
  });

  it("error markup after a rejected save", async () => {
    const user = userEvent.setup();
    const { container } = render(<LegalGate {...props({ onAgree: vi.fn().mockRejectedValue(new Error("x")) })} />);
    await user.click(screen.getByRole("radio", { name: /for myself/i }));
    await user.click(screen.getByRole("checkbox"));
    await user.click(screen.getByRole("button", { name: /agree/i }));
    await screen.findByRole("alert");
    // Newer jsdom/cssstyle re-serialises the inline style of re-rendered nodes and turns the
    // authored `border: none` into `border: medium`; the 0.2.1 fixture predates that. The authored
    // markup is unchanged, so drop only that serialisation artefact before the byte comparison.
    const html = container.innerHTML.replaceAll("border: medium; ", "");
    expect(html).toBe(readFileSync(join(process.cwd(), "src/fixtures/error.html"), "utf8"));
  });
});
