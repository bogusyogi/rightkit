import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import MarkdownText from "./MarkdownText";

describe("MarkdownText", () => {
  it("renders markdown tables as semantic transcript tables", () => {
    const html = renderToStaticMarkup(
      <MarkdownText
        text={[
          "Before",
          "",
          "| File | Status |",
          "| --- | :---: |",
          "| `App.tsx` | **ready** |",
          "",
          "After",
        ].join("\n")}
      />,
    );

    expect(html).toContain("<table");
    expect(html).toContain("<thead");
    expect(html).toContain("<tbody");
    expect(html).toContain("<th");
    expect(html).toContain("<td");
    expect(html).toContain("<code");
    expect(html).toContain("<strong");
    expect(html).toContain("App.tsx");
    expect(html).toContain("ready");
    expect(html).not.toContain("| File | Status |");
  });

  it("keeps escaped pipes inside table cells", () => {
    const html = renderToStaticMarkup(
      <MarkdownText
        text={[
          "| Pattern | Meaning |",
          "| --- | --- |",
          String.raw`| foo\|bar | literal pipe |`,
        ].join("\n")}
      />,
    );

    expect(html).toContain("<table");
    expect(html).toContain("foo|bar");
    expect(html).toContain("literal pipe");
  });

  it("renders bold spans that contain inline code without leaking literal asterisks", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="Point 2. **`memrightPlannerStatus` reconciles to live receipts, not to help text** — already fixed." />,
    );

    expect(html).toContain("<strong");
    expect(html).toContain("<code");
    expect(html).toContain("memrightPlannerStatus");
    expect(html).not.toContain("**");
  });

  it("renders URLs as external links, not inert spans", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="See [release notes](https://docs.anthropic.com/en/release-notes/api) and https://example.com/page" />,
    );

    expect(html).toContain("<a ");
    expect(html).toContain('href="https://docs.anthropic.com/en/release-notes/api"');
    expect(html).toContain('href="https://example.com/page"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("autolinks mailto: addresses inside [label](...) but not bare", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="Reach [support](mailto:support@example.com) or copy support@example.com" />,
    );

    expect(html).toContain('href="mailto:support@example.com"');
    expect(html).toContain("support");
    // Bare email must NOT become a link (only the explicit [label](mailto:) form).
    expect(html).not.toContain('href="mailto:support@example.com">support@example.com<');
  });

  it("gives inline code a quiet neutral background for scanning", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="Read `package.json`, then inspect `name` and `scripts`." />,
    );

    expect(html).toContain("font-mono");
    expect(html).toContain("bg-panel");
    expect(html).toContain("rounded-sm");
    expect(html).not.toContain("text-accent-2");
    expect(html).not.toContain("border-line");
  });

  it("keeps transcript tables at 13px", () => {
    const html = renderToStaticMarkup(
      <MarkdownText
        text={[
          "| File | Status |",
          "| --- | --- |",
          "| App.tsx | ready |",
        ].join("\n")}
      />,
    );

    expect(html).toContain("text-[13px]");
  });

  it("renders mailto: links in [label](mailto:...) form", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="Contact [Adrian](mailto:adrdsouza@gmail.com) for help" />,
    );

    expect(html).toContain("<a ");
    expect(html).toContain('href="mailto:adrdsouza@gmail.com"');
    expect(html).toContain('rel="noopener noreferrer"');
  });

  it("does not autolink bare mailto URLs or javascript: pseudo-protocols", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text="[bad](javascript:alert(1)) and [ok](mailto:bob@example.com)" />,
    );

    expect(html).not.toContain('href="javascript:');
    expect(html).toContain('href="mailto:bob@example.com"');
  });

  it("uses semantic headings and preserves fenced code safely", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text={'# Title\n\n```ts\nconst value = "<safe>";\n```'} />,
    );

    expect(html).toContain("<h1");
    expect(html).toContain("<code");
    expect(html).toContain("&lt;safe&gt;");
    expect(html).not.toContain("<script");
  });

  it("keeps an unterminated fence contained in a code block", () => {
    const html = renderToStaticMarkup(<MarkdownText text={'```\n**not emphasis**'} />);

    expect(html).toContain("<pre");
    expect(html).toContain("**not emphasis**");
    expect(html).not.toContain("<strong");
  });

  it("renders GFM task items and a non-default ordered-list start", () => {
    const html = renderToStaticMarkup(
      <MarkdownText text={"3. first\n4. second\n\n- [x] shipped\n- [ ] pending"} />,
    );

    expect(html).toContain('start="3"');
    expect(html).toContain('aria-label="Completed task"');
    expect(html).toContain('aria-label="Incomplete task"');
  });
});
