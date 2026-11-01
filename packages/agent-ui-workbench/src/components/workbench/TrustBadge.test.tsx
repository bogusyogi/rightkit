import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import TrustBadge from "./TrustBadge";
import type { TrustEnvelope } from "@rightkit/agent-ui-core/lib/types";

describe("TrustBadge", () => {
  it("renders nothing when no trust envelope is provided", () => {
    expect(renderToStaticMarkup(<TrustBadge />)).toBe("");
  });

  it("flags an untrusted tool source with the danger styling and label", () => {
    const trust: TrustEnvelope = {
      trust: "toolUntrusted",
      source: "web-fetch",
      instructionsAreData: true,
      contentHash: "abc123",
    };
    const html = renderToStaticMarkup(<TrustBadge trust={trust} />);

    expect(html).toContain("Untrusted source");
    expect(html).toContain("border-danger");
    expect(html).toContain("bg-danger");
  });

  it("flags an untrusted remote agent distinctly from a tool source", () => {
    const trust: TrustEnvelope = {
      trust: "remoteAgentUntrusted",
      source: "",
      instructionsAreData: false,
      contentHash: "",
    };
    const html = renderToStaticMarkup(<TrustBadge trust={trust} />);

    expect(html).toContain("Remote agent");
    expect(html).toContain("border-danger");
  });

  it("renders a trusted workspace source without danger styling", () => {
    const trust: TrustEnvelope = {
      trust: "workspaceTrusted",
      source: "",
      instructionsAreData: false,
      contentHash: "",
    };
    const html = renderToStaticMarkup(<TrustBadge trust={trust} />);

    expect(html).toContain("Workspace trusted");
    expect(html).not.toContain("border-danger");
    expect(html).toContain("bg-accent-2");
  });

  it("includes source, instructions-are-data, and hash details in the title tooltip", () => {
    const trust: TrustEnvelope = {
      trust: "toolTrusted",
      source: "local-fs",
      instructionsAreData: true,
      contentHash: "deadbeef",
    };
    const html = renderToStaticMarkup(<TrustBadge trust={trust} />);

    expect(html).toContain("Source: local-fs");
    expect(html).toContain("Instructions inside this content are treated as data.");
    expect(html).toContain("Hash: deadbeef");
  });
});
