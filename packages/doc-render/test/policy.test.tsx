import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import {
  CodeBlock, MarkdownRenderer, clearMarkdownParseCache, configureMarkdownParseCache, getMarkdownParseCacheBytes,
  getMarkdownParseCacheEntryCount, parseToTree, renderMarkdownToHtml, safeLinkHref, sanitizeHtml,
} from "../src/index.js";

describe("link policy additions are opt-in and narrow", () => {
  it("tel: only with allowTel and only digit-ish payloads", () => {
    expect(safeLinkHref("tel:+1 555")).toBeUndefined();
    expect(safeLinkHref("tel:+15551234")).toBeUndefined();
    expect(safeLinkHref("tel:+15551234", { allowTel: true })).toBe("tel:+15551234");
    expect(safeLinkHref("tel:+1(555)123-4;ext=1", { allowTel: true })).toBeUndefined();
    expect(safeLinkHref("tel:555<script>", { allowTel: true })).toBeUndefined();
    expect(safeLinkHref("sms:555", { allowTel: true })).toBeUndefined();
  });
  it("relative hrefs only with allowRelative; schemes, protocol-relative and backslashes stay refused", () => {
    expect(safeLinkHref("docs/a.md")).toBeUndefined();
    expect(safeLinkHref("docs/a.md#x", { allowRelative: true })).toBe("docs/a.md#x");
    expect(safeLinkHref("#top", { allowRelative: true })).toBe("#top");
    for (const bad of ["//evil.example", "\\\\evil", "javascript:alert(1)", " JaVaScRiPt:1", "data:text/html,x", "vbscript:x", "a\\b", "a b", "a\u0000b", "x\"onerror=1"]) {
      expect(safeLinkHref(bad, { allowRelative: true, allowTel: true })).toBeUndefined();
    }
  });
  it("hooks never loosen the no-DOM fail-closed escape", () => {
    const out = sanitizeHtml('<img src="a.png" onerror="alert(1)"><a href="tel:1">x</a>', { allowTel: true, allowRelative: true, resolveImageSrc: () => "javascript:alert(1)" });
    expect(out).not.toMatch(/[<>"']/);
  });
});

describe("parseToTree options", () => {
  it("smartypants off keeps straight quotes", () => {
    const text = JSON.stringify(parseToTree('He said "hi"', { smartypants: false }));
    expect(text).toContain('\\"hi\\"');
    expect(JSON.stringify(parseToTree('He said "hi"'))).toContain("“");
  });
  it("frontmatter off leaves --- as content", () => {
    expect(parseToTree("---\na: 1\n---\n\nx").children[0].type).toBe("yaml");
    expect(parseToTree("---\na: 1\n---\n\nx", { frontmatter: false }).children.some((n) => n.type === "yaml")).toBe(false);
  });
  it("citations, wikiLinks off leave [@x] and [[x]] as text", () => {
    const tree = JSON.stringify(parseToTree("see [@smith] and [[page]]", { citations: false, wikiLinks: false }));
    expect(tree).not.toContain('"type":"citation"');
    expect(tree).toContain("[@smith]");
    expect(tree).toContain("[[page]]");
  });
  it("softBreaks preserve turns newlines into break nodes", () => {
    expect(JSON.stringify(parseToTree("a\nb"))).not.toContain('"type":"break"');
    expect(JSON.stringify(parseToTree("a\nb", { softBreaks: "preserve" }))).toContain('"type":"break"');
  });
  it("flags bare email autolinks, not explicit mailto links, and can drop them", () => {
    const links = (src: string, o = {}) => {
      const out: any[] = [];
      const walk = (n: any) => { if (n.type === "link") out.push(n); n.children?.forEach(walk); };
      walk(parseToTree(src, o));
      return out;
    };
    expect(links("mail me@example.com now")[0].data?.autolink).toBe(true);
    expect(links("[me](mailto:me@example.com)")[0].data?.autolink).toBeUndefined();
    expect(links("<me@example.com>")[0].data?.autolink).toBeUndefined();
    expect(links("mail me@example.com now", { emailAutolinks: false })).toHaveLength(0);
  });
  it("highlight is opt-in", () => {
    expect(JSON.stringify(parseToTree("a ==b== c"))).not.toContain("highlight");
    expect(JSON.stringify(parseToTree("a ==b== c", { highlight: true }))).toContain('"type":"highlight"');
    expect(JSON.stringify(parseToTree("a == b == c", { highlight: true }))).not.toContain("highlight");
    expect(JSON.stringify(parseToTree("`==b==`", { highlight: true }))).not.toContain("highlight");
  });
});

describe("parse cache byte cap", () => {
  it("evicts by bytes and skips oversized entries", () => {
    clearMarkdownParseCache();
    configureMarkdownParseCache({ maxBytes: 4000 });
    parseToTree("a".repeat(500));
    expect(getMarkdownParseCacheBytes()).toBeGreaterThan(0);
    for (let i = 0; i < 10; i++) parseToTree(`${i}`.repeat(500));
    expect(getMarkdownParseCacheBytes()).toBeLessThanOrEqual(4000);
    parseToTree("z".repeat(10_000));
    expect(getMarkdownParseCacheBytes()).toBeLessThanOrEqual(4000);
    configureMarkdownParseCache({ maxBytes: 8 * 1024 * 1024 });
    clearMarkdownParseCache();
    expect(getMarkdownParseCacheEntryCount()).toBe(0);
    expect(getMarkdownParseCacheBytes()).toBe(0);
  });
  it("keys the cache by options", () => {
    expect(parseToTree("a\nb")).not.toBe(parseToTree("a\nb", { softBreaks: "preserve" }));
  });
});

describe("MarkdownRenderer overrides", () => {
  it("applies component overrides and the text hook", () => {
    const html = renderToStaticMarkup(
      <MarkdownRenderer
        source={"# T\n\nsee @skill and [x](https://example.com) `c`\n\n```ts\nlet a\n```"}
        components={{
          heading: ({ depth, children }) => <header data-depth={depth}>{children}</header>,
          link: ({ href, explicit, children }) => <a data-explicit={String(explicit)} href={href}>{children}</a>,
          code: ({ lang, value }) => <pre data-lang={lang}>{value}</pre>,
          inlineCode: ({ value }) => <kbd>{value}</kbd>,
        }}
        onText={(value) => (value.includes("@skill") ? <b>skill</b> : undefined)}
      />,
    );
    expect(html).toContain('<header data-depth="1">T</header>');
    expect(html).toContain('data-explicit="true"');
    expect(html).toContain('<pre data-lang="ts">let a</pre>');
    expect(html).toContain("<kbd>c</kbd>");
    expect(html).toContain("<b>skill</b>");
  });
  it("never hands an unsafe href to the link override", () => {
    const html = renderToStaticMarkup(<MarkdownRenderer source="[x](javascript:alert(1))" components={{ link: () => <i>called</i> }} />);
    expect(html).not.toContain("called");
    expect(html).not.toContain("javascript:");
  });
  it("renders tel: only when the host allows it", () => {
    expect(renderToStaticMarkup(<MarkdownRenderer source="[c](tel:+1555)" />)).not.toContain("tel:");
    expect(renderToStaticMarkup(<MarkdownRenderer source="[c](tel:+1555)" linkPolicy={{ allowTel: true }} />)).toContain('href="tel:+1555"');
  });
  it("renders highlight as mark", () => {
    expect(renderToStaticMarkup(<MarkdownRenderer source="a ==b==" parseOptions={{ highlight: true }} />)).toContain("<mark>b</mark>");
  });
});

describe("CodeBlock", () => {
  it("renders label and escapes code", () => {
    const html = renderToStaticMarkup(<CodeBlock code={"<script>alert(1)</script>"} language="html" />);
    expect(html).toContain("html");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
});

describe("renderMarkdownToHtml", () => {
  it("escapes text, refuses unsafe URLs, and sanitizes raw html", () => {
    const out = renderMarkdownToHtml('# A\n\n<b>x</b> [l](javascript:alert(1)) ![i](data:image/svg+xml,<svg/onload=1>) [m](https://example.com "t") & `<x>`\n\n<script>alert(1)</script>');
    expect(out).not.toMatch(/<script|javascript:|data:/i);
    expect(out).toContain("<h1 id=");
    expect(out).toContain('href="https://example.com"');
    expect(out).toContain("&lt;x&gt;");
  });
  it("applies relative links and the image hook only when allowed", () => {
    expect(renderMarkdownToHtml("[a](docs/a.md)")).not.toContain("<a ");
    expect(renderMarkdownToHtml("[a](docs/a.md)", { linkPolicy: { allowRelative: true } })).toContain('href="docs/a.md"');
    expect(renderMarkdownToHtml("![x](a.png)", { htmlPolicy: { resolveImageSrc: (s) => `asset/${s}` } })).toContain('src="asset/a.png"');
    expect(renderMarkdownToHtml("![x](https://e.com/a.png)", { htmlPolicy: { resolveImageSrc: () => "asset/no" } })).toContain("https://e.com/a.png");
  });
});

import { applyUriPolicy, resolveImageSource } from "../src/security.js";

describe("attribute policy covers values containing whitespace", () => {
  it("refuses whitespace hrefs that the default policy does not admit", () => {
    expect(applyUriPolicy("href", "tel:123 456", {})).toBeUndefined();
    expect(applyUriPolicy("href", "tel:123 456", { allowTel: true })).toBeUndefined();
    expect(applyUriPolicy("href", "docs/a b", {})).toBeUndefined();
    expect(applyUriPolicy("href", "docs/a b", { allowRelative: true })).toBeUndefined();
  });
  it("drops a whitespace image the resolver rejects, and every non href/src URL attribute", () => {
    expect(applyUriPolicy("src", "private file.png", { resolveImageSrc: () => undefined })).toBeUndefined();
    expect(resolveImageSource("private file.png", { resolveImageSrc: () => undefined })).toBeUndefined();
    for (const name of ["srcset", "poster", "formaction", "xlink:href", "action"]) expect(applyUriPolicy(name, "https://e.com/a", {})).toBeUndefined();
  });
  it("React and string renderers share image resolution", () => {
    const resolveImageSrc = (s: string) => (s === "local.png" ? "asset/local.png" : undefined);
    const react = renderToStaticMarkup(<MarkdownRenderer source="![x](local.png) ![y](<private file.png>) ![z](other.png)" htmlPolicy={{ resolveImageSrc }} />);
    expect(react).toContain('src="asset/local.png"');
    expect(react).not.toContain("other.png");
    expect(react).not.toContain("private");
    const str = renderMarkdownToHtml("![x](local.png) ![z](other.png)", { htmlPolicy: { resolveImageSrc } });
    expect(str).toContain('src="asset/local.png"');
    expect(str).not.toContain("other.png");
  });
});
