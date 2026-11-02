import { fireEvent, render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import DocumentPreview from "./DocumentPreview.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

const docRef: ReferenceObject = {
  id: "doc-1",
  kind: "generated_document",
  destination: "/tmp/notes.md",
  mime: "text/markdown",
  filename: "notes.md",
  sizeBytes: 1024,
  provenance: {
    source: "workspace_resolver",
    createdAt: "2026-07-20T00:00:00Z",
    trust: "workspaceTrusted",
  },
  actions: ["open", "preview"],
};

describe("DocumentPreview", () => {
  it("renders an inert placeholder when no resolver is wired", () => {
    const { container } = render(<DocumentPreview reference={docRef} />);
    expect(container.querySelector('[data-document-preview="true"]')).toBeTruthy();
    expect(container.textContent).toContain("Document resolver not wired");
    expect(container.textContent).toContain("/tmp/notes.md");
  });

  it("uses filename as the title when no override is provided", () => {
    const { container } = render(<DocumentPreview reference={docRef} />);
    expect(container.textContent).toContain("notes.md");
  });

  it("switches between Rendered and Source modes via the segmented control", async () => {
    const resolve = vi.fn(() => "# Hello\n\nWorld");
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    const sourceButton = getByText("Source");
    fireEvent.click(sourceButton);
    await waitFor(() => {
      expect(container.querySelector("pre")).toBeTruthy();
    });
    expect(container.textContent).toContain("Hello");
  });

  it("renders MarkdownText when Rendered mode is selected after resolving", async () => {
    const resolve = vi.fn(() => "**bold** body");
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    const renderedButton = getByText("Rendered");
    fireEvent.click(renderedButton);
    await waitFor(() => {
      expect(container.querySelector("strong")).toBeTruthy();
    });
    expect(container.querySelector("strong")?.textContent).toBe("bold");
  });

  it("calls the async resolver with the reference and respects the result", async () => {
    const resolve = vi.fn(() => Promise.resolve("async body"));
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    fireEvent.click(getByText("Source"));
    await waitFor(() => {
      expect(container.textContent).toContain("async body");
    });
    expect(resolve).toHaveBeenCalledWith(docRef);
  });

  it("captures resolver errors and renders them in the danger color", async () => {
    const resolve = vi.fn(() => Promise.reject(new Error("boom")));
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    fireEvent.click(getByText("Source"));
    await waitFor(() => {
      expect(container.textContent).toContain("boom");
    });
  });

  it("strips HTML-ish tags before rendering the resolved body as Markdown", async () => {
    const resolve = vi.fn(() => "<script>alert(1)</script>real content");
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    fireEvent.click(getByText("Rendered"));
    await waitFor(() => {
      expect(container.textContent).toContain("real content");
    });
    expect(container.innerHTML).not.toContain("<script");
  });

  it("exposes aria-label on the document region", () => {
    const { container } = render(<DocumentPreview reference={docRef} />);
    const region = container.querySelector("section[aria-label]");
    expect(region?.getAttribute("aria-label")).toContain("Document preview:");
  });

  it("reserves a minimum height on the body so async content does not shift the layout", async () => {
    const resolve = vi.fn(() => Promise.resolve("line one\nline two\nline three"));
    const { container, getByText } = render(
      <DocumentPreview reference={docRef} resolve={resolve} />,
    );
    const body = container.querySelector('[aria-live="polite"]');
    expect(body?.className).toContain("min-h-[4rem]");
    fireEvent.click(getByText("Source"));
    await waitFor(() => {
      expect(container.textContent).toContain("line one");
    });
    expect(container.querySelector('[aria-live="polite"]')?.className).toContain("min-h-[4rem]");
  });
});
