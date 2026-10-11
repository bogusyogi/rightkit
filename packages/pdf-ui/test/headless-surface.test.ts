// 0.1.6 headless surface: loader options, text-content source, span highlighting, and the exports
// ViewRight-style hosts compose into their own viewer.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import * as surface from "../src/index.js";
import { pdfDocumentInit, type PdfDocumentOptions } from "../src/loader.js";
import { pdfTextContentSource } from "../src/textLayer.js";
import { highlightSpan, MARK_ACTIVE_BG, MARK_BG } from "../src/shared.js";

describe("headless exports", () => {
  it("exports the password handler and load timer used by PdfViewer", () => {
    expect(typeof surface.attachPasswordHandler).toBe("function");
    expect(typeof surface.createLoadTimer).toBe("function");
  });
  it("exports the loader, text-layer and find-mark helpers", () => {
    expect(typeof surface.pdfDocumentInit).toBe("function");
    expect(typeof surface.pdfTextContentSource).toBe("function");
    expect(typeof surface.renderPdfTextLayer).toBe("function");
  });
});

describe("pdfDocumentInit", () => {
  it("drops a range transport from options so the url source stays the source", () => {
    const init = pdfDocumentInit("https://example.test/a.pdf", { range: {} as never, wasmUrl: "/w/" } as never);
    expect(init).toEqual({ url: "https://example.test/a.pdf", wasmUrl: "/w/" });
  });

  const bytes = new Uint8Array([37, 80, 68, 70]);

  it("is exactly the historical init when no options are given", () => {
    expect(pdfDocumentInit("https://example.test/a.pdf")).toEqual({ url: "https://example.test/a.pdf" });
    expect(pdfDocumentInit(bytes)).toEqual({ data: bytes });
    expect(pdfDocumentInit(bytes, undefined)).toEqual({ data: bytes });
    expect(pdfDocumentInit(bytes, {})).toEqual({ data: bytes });
  });

  it("passes pdf.js getDocument options through beside the source", () => {
    const options: PdfDocumentOptions = { wasmUrl: "/pdfjs/", cMapUrl: "/cmaps/", cMapPacked: true, standardFontDataUrl: "/fonts/" };
    expect(pdfDocumentInit(bytes, options)).toEqual({ data: bytes, ...options });
    expect(pdfDocumentInit("/a.pdf", options)).toEqual({ url: "/a.pdf", ...options });
  });

  it("never lets options replace the source", () => {
    const hostile = { url: "/other.pdf", data: new Uint8Array([1]) } as unknown as PdfDocumentOptions;
    expect(pdfDocumentInit(bytes, hostile)).toEqual({ data: bytes });
    expect(pdfDocumentInit("/a.pdf", hostile)).toEqual({ url: "/a.pdf" });
  });
});

describe("pdfTextContentSource", () => {
  function fakePage() {
    const calls: string[] = [];
    const stream = { kind: "stream" } as unknown as ReadableStream;
    const page = {
      getTextContent: async () => { calls.push("getTextContent"); return { items: [], styles: {} }; },
      streamTextContent: () => { calls.push("streamTextContent"); return stream; },
    };
    return { page: page as unknown as Parameters<typeof pdfTextContentSource>[0], calls, stream };
  }

  it("defaults to getTextContent, as the viewer always has", async () => {
    const { page, calls } = fakePage();
    await pdfTextContentSource(page);
    expect(calls).toEqual(["getTextContent"]);
  });

  it("uses streamTextContent when asked, and resolves to the stream itself", async () => {
    const { page, calls, stream } = fakePage();
    await expect(pdfTextContentSource(page, "streamTextContent")).resolves.toBe(stream);
    expect(calls).toEqual(["streamTextContent"]);
  });
});

// Minimal DOM covering the calls highlightSpan makes; the package has no DOM test environment.
class FakeText { constructor(readonly data: string) {} get textContent() { return this.data; } }
class FakeFragment { children: Array<FakeText | FakeMark> = []; appendChild(node: FakeText | FakeMark) { this.children.push(node); return node; } }
class FakeMark {
  style = { cssText: "" };
  children: FakeText[] = [];
  constructor(readonly tag: string) {}
  get textContent() { return this.children.map((c) => c.data).join(""); }
  set textContent(value: string) { this.children = [new FakeText(value)]; }
}
class FakeSpan {
  children: Array<FakeText | FakeMark> = [];
  get textContent() { return this.children.map((c) => c.textContent).join(""); }
  get childElementCount() { return this.children.filter((c) => c instanceof FakeMark).length; }
  replaceChildren(...nodes: Array<FakeText | FakeMark | FakeFragment>) {
    this.children = nodes.flatMap((n) => (n instanceof FakeFragment ? n.children : [n]));
  }
}

describe("highlightSpan", () => {
  let previous: unknown;
  beforeEach(() => {
    previous = (globalThis as { document?: unknown }).document;
    (globalThis as { document?: unknown }).document = {
      createTextNode: (data: string) => new FakeText(data),
      createDocumentFragment: () => new FakeFragment(),
      createElement: (tag: string) => new FakeMark(tag),
    };
  });
  afterEach(() => { (globalThis as { document?: unknown }).document = previous; });

  function span(text: string) { const s = new FakeSpan(); s.children = [new FakeText(text)]; return s; }
  function marks(s: FakeSpan) { return s.children.filter((c): c is FakeMark => c instanceof FakeMark).map((m) => ({ text: m.textContent, css: m.style.cssText })); }

  it("keeps the default output: every match marked with MARK_BG", () => {
    const s = span("one two one");
    highlightSpan(s as unknown as HTMLElement, "one");
    expect(marks(s)).toEqual([
      { text: "one", css: `background:${MARK_BG};color:inherit;border-radius:2px` },
      { text: "one", css: `background:${MARK_BG};color:inherit;border-radius:2px` },
    ]);
    expect(s.textContent).toBe("one two one");
  });

  it("uses markBg for every match and markActiveBg for the chosen occurrence", () => {
    const s = span("a a a");
    highlightSpan(s as unknown as HTMLElement, "a", { markBg: "#111", markActiveBg: "#222", activeOccurrence: 1 });
    expect(marks(s).map((m) => m.css)).toEqual([
      "background:#111;color:inherit;border-radius:2px",
      "background:#222;color:inherit;border-radius:2px",
      "background:#111;color:inherit;border-radius:2px",
    ]);
  });

  it("ignores a colour that could add a declaration or fetch a resource", () => {
    const s = span("a a");
    highlightSpan(s as unknown as HTMLElement, "a", { markBg: "red;position:fixed;inset:0", markActiveBg: "url(https://x.test/a)", activeOccurrence: 0 });
    expect(marks(s).map((m) => m.css)).toEqual([
      `background:${MARK_ACTIVE_BG};color:inherit;border-radius:2px`,
      `background:${MARK_BG};color:inherit;border-radius:2px`,
    ]);
  });

  it("defaults markActiveBg to MARK_ACTIVE_BG when an occurrence is active", () => {
    const s = span("x x");
    highlightSpan(s as unknown as HTMLElement, "x", { activeOccurrence: 0 });
    expect(marks(s)[0]?.css).toBe(`background:${MARK_ACTIVE_BG};color:inherit;border-radius:2px`);
    expect(marks(s)[1]?.css).toBe(`background:${MARK_BG};color:inherit;border-radius:2px`);
  });

  it("is idempotent across re-runs and flattens a span with no match", () => {
    const s = span("cat dog");
    highlightSpan(s as unknown as HTMLElement, "dog");
    highlightSpan(s as unknown as HTMLElement, "dog", { activeOccurrence: 0 });
    expect(marks(s)).toHaveLength(1);
    expect(s.textContent).toBe("cat dog");
    highlightSpan(s as unknown as HTMLElement, "zebra");
    expect(marks(s)).toHaveLength(0);
    expect(s.textContent).toBe("cat dog");
  });
});
