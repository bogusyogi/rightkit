// @vitest-environment node
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import RunAnnouncer from "./RunAnnouncer.js";

// The transcript renders new messages with no live region, so a screen-reader
// user got silence — no signal the agent started, none when the answer landed.
describe("RunAnnouncer", () => {
  it("exposes a polite live region", () => {
    const html = renderToStaticMarkup(<RunAnnouncer messages={[]} isWorking={false} />);
    expect(html).toContain('aria-live="polite"');
    expect(html).toContain('aria-atomic="true"');
    expect(html).toContain("sr-only");
  });

  it("only speaks completed assistant turns, never streaming tokens", () => {
    const source = readFileSync(new URL("./RunAnnouncer.tsx", import.meta.url), "utf8");
    // Streaming token-by-token would be an unusable firehose.
    expect(source).toMatch(/if \(isWorking\) return;/);
    // Long replies stay readable in the transcript rather than being read out.
    expect(source).toContain("response continues in the transcript");
  });
});
