// @vitest-environment node
import { describe, expect, it } from "vitest";

import { isNetworkFailureMessage, sanitizeError } from "@rightkit/agent-ui-hooks/components/sanitizeError";

describe("sanitizeError", () => {
  it("replaces a raw browser network failure with the caller's subsystem fallback", () => {
    // WebKit's fetch TypeError. Rendered verbatim it produced a bare
    // "Load failed" beside a green health dot in the Memory pane.
    const { summary, details } = sanitizeError(
      new TypeError("Load failed"),
      "Memory store unavailable.",
    );
    expect(summary).toBe("Memory store unavailable.");
    expect(details).toContain("Load failed");
    expect(details).toContain("did not reach the engine");
  });

  it("treats every engine's wording for the same failure alike", () => {
    for (const message of [
      "Load failed",
      "Failed to fetch",
      "NetworkError when attempting to fetch resource.",
      "Network request failed",
    ]) {
      expect(isNetworkFailureMessage(message)).toBe(true);
    }
    expect(isNetworkFailureMessage("load failed while parsing plugin manifest")).toBe(false);
    expect(isNetworkFailureMessage("workspace tree unavailable")).toBe(false);
  });

  it("keeps a real engine message as the summary", () => {
    const { summary } = sanitizeError(
      new Error("memory store rejected the query: invalid tier"),
      "Memory store unavailable.",
    );
    expect(summary).toBe("memory store rejected the query: invalid tier");
  });

  it("falls back when the error carries no message", () => {
    expect(sanitizeError(null, "Memory store unavailable.").summary).toBe(
      "Memory store unavailable.",
    );
  });
});
