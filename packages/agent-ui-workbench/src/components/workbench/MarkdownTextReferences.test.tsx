import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";
import MarkdownText from "./MarkdownText.js";
import {
  buildInlineReferenceResolver,
  setInlineReferenceResolver,
} from "./references/referenceRegistry.js";

const fileRef: ReferenceObject = {
  id: "ref-file-1",
  kind: "workspace_file",
  destination: "/tmp/notes.md",
  filename: "notes.md",
  mime: "text/markdown",
  provenance: {
    source: "workspace_resolver",
    createdAt: "2026-07-20T00:00:00Z",
    trust: "workspaceTrusted",
  },
  actions: ["open", "preview"],
};

const skillRef: ReferenceObject = {
  id: "skill:blueprint",
  kind: "skill",
  destination: "@blueprint",
  filename: "Blueprint",
  provenance: {
    source: "/skills/blueprint/SKILL.md",
    createdAt: "2026-07-21T00:00:00Z",
    trust: "workspaceTrusted",
  },
  actions: [],
};

const render = (text: string) => renderToStaticMarkup(<MarkdownText text={text} />);

describe("MarkdownText inline-reference resolution", () => {
  afterEach(() => {
    setInlineReferenceResolver(null);
  });

  it("renders an inert span for an unknown path", () => {
    setInlineReferenceResolver(buildInlineReferenceResolver([]));
    const html = render("see /tmp/unknown.txt");
    expect(html).toContain("/tmp/unknown.txt");
    expect(html).toContain("decoration-dotted");
    expect(html).not.toContain('data-reference-id="');
  });

  it("renders a ReferenceLink when the path matches a known reference", () => {
    setInlineReferenceResolver(buildInlineReferenceResolver([fileRef]));
    const html = render("see /tmp/notes.md please");
    expect(html).toContain('data-reference-id="ref-file-1"');
    expect(html).toContain('data-reference-kind="workspace_file"');
    expect(html).not.toContain('class="font-mono text-text underline decoration-dotted');
  });

  it("resolves a reference via filename, not just destination", () => {
    setInlineReferenceResolver(buildInlineReferenceResolver([fileRef]));
    expect(render("read notes.md here")).toContain('data-reference-id="ref-file-1"');
  });

  it("upgrades a GitHub repository root to a compact repository object", () => {
    const html = render("see https://github.com/stablyai/orca");
    expect(html).toContain('data-rich-link="repository"');
    expect(html).toContain("stablyai/orca");
    expect(html).toContain('href="https://github.com/stablyai/orca"');
  });

  it("keeps non-repository GitHub URLs as ordinary external links", () => {
    for (const url of [
      "https://github.com/stablyai",
      "https://github.com/stablyai/orca/issues/1",
      "https://github.com/stablyai/orca/tree/main",
      "https://github.com.evil.test/stablyai/orca",
    ]) {
      const html = render(url);
      expect(html).toContain('data-rich-link="external"');
      expect(html).not.toContain('data-rich-link="repository"');
    }
  });

  it("renders only registered skill mentions as semantic skill objects", () => {
    setInlineReferenceResolver(buildInlineReferenceResolver([skillRef]));
    const known = render("use @blueprint");
    const unknown = render("ask @adrian");
    expect(known).toContain('data-reference-kind="skill"');
    expect(known).toContain('data-rich-link="skill"');
    expect(unknown).toContain("@adrian");
    expect(unknown).not.toContain('data-rich-link="skill"');
  });

  it("does not upgrade repository URLs or skill mentions inside inline code", () => {
    setInlineReferenceResolver(buildInlineReferenceResolver([skillRef]));
    const html = render("`https://github.com/stablyai/orca` and `@blueprint`");
    expect(html).toContain("<code");
    expect(html).not.toContain('data-rich-link="repository"');
    expect(html).not.toContain('data-rich-link="skill"');
  });

  it("supports nested emphasis and strikethrough without exposing markdown markers", () => {
    const html = render("**ready `now`** and ~~old _copy_~~");
    expect(html).toContain("<strong");
    expect(html).toContain("<del");
    expect(html).toContain("<em");
    expect(html).not.toContain("~~");
  });
});
