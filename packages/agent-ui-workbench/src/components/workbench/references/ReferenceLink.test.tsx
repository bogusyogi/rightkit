import { fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ReferenceLink from "./ReferenceLink.js";
import { setInlineReferenceHostAction } from "./referenceRegistry.js";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

// Mock the secureStore so we can assert the destination was passed through.
const openExternalMock = vi.fn(async (_url: string) => {});
vi.mock("@rightkit/agent-ui-core/lib/secureStore", () => ({
  openExternal: (url: string) => openExternalMock(url),
}));

const extRef: ReferenceObject = {
  id: "ext-1",
  kind: "external_link",
  destination: "https://example.com/page",
  provenance: {
    source: "test",
    createdAt: "2026-07-20T00:00:00Z",
    trust: "user",
  },
  actions: ["open"],
};

const fileRef: ReferenceObject = {
  id: "file-1",
  kind: "workspace_file",
  destination: "/tmp/foo.txt",
  filename: "foo.txt",
  provenance: {
    source: "test",
    createdAt: "2026-07-20T00:00:00Z",
    trust: "workspaceTrusted",
  },
  actions: ["open", "preview"],
};

describe("ReferenceLink", () => {
  beforeEach(() => {
    openExternalMock.mockClear();
    setInlineReferenceHostAction(null);
  });
  afterEach(() => {
    setInlineReferenceHostAction(null);
  });

  it("renders an <a> for an external_link without onAction that opens via secureStore", () => {
    const { container } = render(<ReferenceLink reference={extRef} />);
    const a = container.querySelector("a");
    expect(a).toBeTruthy();
    expect(a?.getAttribute("href")).toBe("https://example.com/page");
    expect(a?.getAttribute("rel")).toBe("noopener noreferrer");
    expect(a?.getAttribute("data-reference-id")).toBe("ext-1");
    expect(a?.getAttribute("data-reference-kind")).toBe("external_link");
    if (!a) throw new Error("external-link anchor missing");
    fireEvent.click(a);
    expect(openExternalMock).toHaveBeenCalledWith("https://example.com/page");
  });

  it("renders a button for non-link kinds when onAction is supplied", () => {
    const onAction = vi.fn();
    const { container } = render(<ReferenceLink reference={fileRef} onAction={onAction} />);
    const btn = container.querySelector("button");
    expect(btn).toBeTruthy();
    expect(btn?.getAttribute("data-reference-id")).toBe("file-1");
    expect(btn?.getAttribute("data-reference-kind")).toBe("workspace_file");
    if (!btn) throw new Error("reference action button missing");
    fireEvent.click(btn);
    expect(onAction).toHaveBeenCalledWith("file-1", "preview");
  });

  it("prefers preview over open when both actions are listed", () => {
    const onAction = vi.fn();
    const { container } = render(<ReferenceLink reference={fileRef} onAction={onAction} />);
    const button = container.querySelector("button");
    if (!button) throw new Error("reference preview button missing");
    fireEvent.click(button);
    expect(onAction).toHaveBeenCalledWith("file-1", "preview");
  });

  it("blocks unsafe schemes by rendering an inert danger span", () => {
    const unsafe: ReferenceObject = {
      ...extRef,
      destination: "javascript:alert(1)",
    };
    const { container } = render(<ReferenceLink reference={unsafe} />);
    expect(container.querySelector("a")).toBeNull();
    const span = container.querySelector('[data-reference-unsafe="true"]');
    expect(span).toBeTruthy();
    expect(span?.textContent).toContain("javascript:");
    if (!span) throw new Error("unsafe-link guard missing");
    fireEvent.click(span);
    expect(openExternalMock).not.toHaveBeenCalled();
  });

  it("blocks vbscript: and file: schemes too", () => {
    for (const destination of ["vbscript:msgbox(1)", "file:///etc/passwd"]) {
      const { container } = render(<ReferenceLink reference={{ ...extRef, destination }} />);
      expect(container.querySelector("a")).toBeNull();
      expect(container.querySelector('[data-reference-unsafe="true"]')).toBeTruthy();
    }
  });

  it("renders a danger span when the reference carries an error", () => {
    const errored: ReferenceObject = { ...fileRef, error: "missing" };
    const { container } = render(<ReferenceLink reference={errored} onAction={() => {}} />);
    const span = container.querySelector('[data-reference-error="true"]');
    expect(span).toBeTruthy();
    expect(span?.getAttribute("title")).toBe("missing");
  });

  it("renders an inert span as a last-resort when no onAction and not external_link", () => {
    const noActionRef: ReferenceObject = {
      ...fileRef,
      actions: [],
    };
    const { container } = render(<ReferenceLink reference={noActionRef} />);
    const span = container.querySelector("span");
    expect(span).toBeTruthy();
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("button")).toBeNull();
  });

  it("uses label override when provided", () => {
    const { container } = render(<ReferenceLink reference={fileRef} label="custom label" />);
    expect(container.textContent).toContain("custom label");
  });

  it("falls back to filename, then destination for the display label", () => {
    const refNoFilename: ReferenceObject = {
      ...fileRef,
      filename: undefined,
    };
    const { container } = render(<ReferenceLink reference={refNoFilename} />);
    expect(container.textContent).toContain("/tmp/foo.txt");
  });

  it("opens repository references through the secure external-link boundary", () => {
    const repository: ReferenceObject = {
      ...extRef,
      id: "repo-1",
      kind: "repository",
      destination: "https://github.com/stablyai/orca",
      filename: "stablyai/orca",
    };
    const { container } = render(<ReferenceLink reference={repository} />);
    const anchor = container.querySelector("a");
    expect(anchor?.getAttribute("data-reference-kind")).toBe("repository");
    if (!anchor) throw new Error("repository anchor missing");
    fireEvent.click(anchor);
    expect(openExternalMock).toHaveBeenCalledWith("https://github.com/stablyai/orca");
  });

  it("blocks unsafe repository destinations", () => {
    const repository: ReferenceObject = {
      ...extRef,
      id: "repo-unsafe",
      kind: "repository",
      destination: "javascript:alert(1)",
      filename: "not/a-repo",
    };
    const { container } = render(<ReferenceLink reference={repository} />);
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector('[data-reference-unsafe="true"]')).toBeTruthy();
  });

  it("renders a skill reference as an inert semantic object", () => {
    const skill: ReferenceObject = {
      ...fileRef,
      id: "skill:blueprint",
      kind: "skill",
      destination: "@blueprint",
      filename: "Blueprint",
      actions: [],
    };
    const { container } = render(<ReferenceLink reference={skill} />);
    expect(container.querySelector('[data-reference-kind="skill"]')).toBeTruthy();
    expect(container.querySelector('[data-rich-link="skill"]')).toBeTruthy();
    expect(container.querySelector("a,button")).toBeNull();
    expect(container.textContent).toContain("Blueprint");
  });
});
