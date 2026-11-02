// D1 — MarkdownArtifactView component test.

import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import MarkdownArtifactView, { sanitizeHref } from "./MarkdownArtifactView.js";
import { rightKitShortcuts } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";
import type { WorkspaceArtifact } from "@rightkit/agent-ui-core/lib/types/workspace-files";

afterEach(cleanup);

const SAMPLE_ARTIFACT: WorkspaceArtifact = {
  id: "readme:README.md",
  kind: "readme",
  path: "README.md",
  title: "README",
  provenance: "workspace-root",
  verdict: null,
  fingerprint: "deadbeef",
  sizeBytes: 64,
  createdAt: "2024-01-01T00:00:00Z",
};

describe("MarkdownArtifactView", () => {
  it("renders the title and renders paragraphs in rendered mode", () => {
    render(<MarkdownArtifactView config={null} workspaceRoot="/root" artifact={SAMPLE_ARTIFACT} />);
    expect(screen.getByTestId("markdown-artifact")).toBeTruthy();
  });

  it("toggles to source mode when the toggle button is clicked", () => {
    render(<MarkdownArtifactView config={null} workspaceRoot="/root" artifact={SAMPLE_ARTIFACT} />);
    fireEvent.click(screen.getByTestId("markdown-artifact-toggle"));
    expect(screen.getByTestId("markdown-artifact-toggle").textContent).toBe("Rendered");
  });

  it("toggles source through its registered shortcut and unregisters on unmount", () => {
    const { unmount } = render(
      <MarkdownArtifactView config={null} workspaceRoot="/root" artifact={SAMPLE_ARTIFACT} />,
    );
    expect(rightKitShortcuts.chordFor("markdown.toggle_view")).toBe("ctrlormeta+shift+m");
    act(() => {
      rightKitShortcuts.invoke("markdown.toggle_view");
    });
    expect(screen.getByTestId("markdown-artifact-toggle").textContent).toBe("Rendered");
    unmount();
    expect(rightKitShortcuts.chordFor("markdown.toggle_view")).toBeUndefined();
  });

  it("exposes a reveal button for the owning surface", () => {
    let captured: string | null = null;
    render(
      <MarkdownArtifactView
        config={null}
        workspaceRoot="/root"
        artifact={SAMPLE_ARTIFACT}
        onReveal={(path) => {
          captured = path;
        }}
      />,
    );
    fireEvent.click(screen.getByTestId("markdown-artifact-reveal"));
    expect(captured).toBe("README.md");
  });
});

describe("sanitizeHref", () => {
  it("passes http and https through unchanged", () => {
    expect(sanitizeHref("https://example.com/path")).toBe("https://example.com/path");
    expect(sanitizeHref("http://example.com")).toBe("http://example.com");
  });

  it("strips real whitespace from safe URLs", () => {
    expect(sanitizeHref("  https://example.com\n")).toBe("https://example.com");
  });

  it("rejects javascript: regardless of case", () => {
    expect(sanitizeHref("javascript:alert(1)")).toBeNull();
    expect(sanitizeHref("JavaScript:alert(1)")).toBeNull();
  });

  it("rejects data:, vbscript:, file:, blob:, and other executable schemes", () => {
    expect(sanitizeHref("data:text/html,<script>alert(1)</script>")).toBeNull();
    expect(sanitizeHref("vbscript:msgbox(1)")).toBeNull();
    expect(sanitizeHref("file:///etc/passwd")).toBeNull();
    expect(sanitizeHref("blob:https://example.com/abc")).toBeNull();
  });

  it("accepts mailto links", () => {
    expect(sanitizeHref("mailto:dev@example.com")).toBe("mailto:dev@example.com");
  });

  it("accepts in-workspace relative paths and anchors", () => {
    expect(sanitizeHref("/api/contract")).toBe("/api/contract");
    expect(sanitizeHref("#installation")).toBe("#installation");
  });

  it("accepts protocol-free relative references composed of safe characters", () => {
    expect(sanitizeHref("docs/pending/ROADMAP.md")).toBe("docs/pending/ROADMAP.md");
    expect(sanitizeHref("../README.md")).toBe("../README.md");
  });

  it("rejects hrefs whose stripped form starts with an unsafe scheme", () => {
    // Real whitespace (tab/newline/CR) is stripped before the scheme
    // check, so an embedded "javascript:" substring collapses onto the
    // URL and is caught by the leading-scheme allowlist.
    expect(sanitizeHref("\tjavascript:alert(1)")).toBeNull();
    expect(sanitizeHref("\njavascript:alert(1)")).toBeNull();
    expect(sanitizeHref("\rjavascript:alert(1)")).toBeNull();
    expect(sanitizeHref(" ")).toBeNull();
    expect(sanitizeHref("")).toBeNull();
  });

  it("rejects unknown protocols", () => {
    expect(sanitizeHref("ftp://example.com")).toBeNull();
    expect(sanitizeHref("custom:foo")).toBeNull();
  });
});
