import { Schema, type Node as PmNode } from "prosemirror-model";
import type { FragmentSourceMap } from "./sourceMap";
import { projectHtmlForVisual, sanitizeHtmlForVisual } from "./htmlVisualSanitize";

import { hasHtmlTextStyle, htmlTextStyleCss, parseHtmlTextStyle } from "./htmlTextStyle";

/**
 * ProseMirror schema for Safe block editing.
 * Mirrors the HTML allowlist: article/section/div, h1-6, p, span, br, hr,
 * strong/em/b/i/s/u/code/pre, a/img, ul/ol/li, blockquote, table etc.
 * Shell (html/head/body/title/meta/link/head style/script) is never projected.
 * All nodes store sourceFrom/sourceTo for precise patching (when available).
 */

function sourceRangeAttrs() {
  return {
    sourceFrom: { default: -1 },
    sourceTo: { default: -1 },
  };
}

export const htmlVisualSchema = new Schema({
  nodes: {
    doc: {
      content: "block+",
      attrs: {
        revision: { default: 0 },
        ...sourceRangeAttrs(),
      },
    },
    paragraph: {
      content: "inline*",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "p" }],
      toDOM() {
        return ["p", 0];
      },
    },
    heading: {
      content: "inline*",
      group: "block",
      attrs: {
        level: { default: 1 },
        ...sourceRangeAttrs(),
      },
      parseDOM: [
        { tag: "h1", attrs: { level: 1 } },
        { tag: "h2", attrs: { level: 2 } },
        { tag: "h3", attrs: { level: 3 } },
        { tag: "h4", attrs: { level: 4 } },
        { tag: "h5", attrs: { level: 5 } },
        { tag: "h6", attrs: { level: 6 } },
      ],
      toDOM(node) {
        return [`h${node.attrs.level}`, 0];
      },
    },
    blockquote: {
      content: "block+",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "blockquote" }],
      toDOM() {
        return ["blockquote", 0];
      },
    },
    article: {
      content: "block+",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "article" }],
      toDOM() {
        return ["article", 0];
      },
    },
    section: {
      content: "block+",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "section" }],
      toDOM() {
        return ["section", 0];
      },
    },
    html_div: {
      content: "block+",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "div" }],
      toDOM() {
        return ["div", 0];
      },
    },
    html_span: {
      content: "inline*",
      group: "inline",
      inline: true,
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "span" }],
      toDOM() {
        return ["span", 0];
      },
    },
    bullet_list: {
      content: "list_item+",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "ul" }],
      toDOM() {
        return ["ul", 0];
      },
    },
    ordered_list: {
      content: "list_item+",
      group: "block",
      attrs: {
        start: { default: 1 },
        ...sourceRangeAttrs(),
      },
      parseDOM: [{ tag: "ol" }],
      toDOM(node) {
        return node.attrs.start && node.attrs.start !== 1
          ? ["ol", { start: node.attrs.start }, 0]
          : ["ol", 0];
      },
    },
    list_item: {
      content: "paragraph block*",
      defining: true,
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "li" }],
      toDOM() {
        return ["li", 0];
      },
    },
    code_block: {
      content: "text*",
      group: "block",
      code: true,
      defining: true,
      marks: "",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "pre" }],
      toDOM() {
        return ["pre", ["code", 0]];
      },
    },
    horizontal_rule: {
      group: "block",
      atom: true,
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "hr" }],
      toDOM() {
        return ["hr"];
      },
    },
    hard_break: {
      inline: true,
      group: "inline",
      selectable: false,
      parseDOM: [{ tag: "br" }],
      toDOM() {
        return ["br"];
      },
    },
    image: {
      inline: true,
      group: "inline",
      draggable: true,
      atom: true,
      attrs: {
        src: { default: "" },
        alt: { default: "" },
        title: { default: "" },
        ...sourceRangeAttrs(),
      },
      parseDOM: [
        {
          tag: "img[src]",
          getAttrs(dom: HTMLElement | string) {
            if (typeof dom === "string") return false;
            return {
              src: dom.getAttribute("src") ?? "",
              alt: dom.getAttribute("alt") ?? "",
              title: dom.getAttribute("title") ?? "",
            };
          },
        },
      ],
      toDOM(node) {
        const { src, alt, title } = node.attrs;
        const attrs: Record<string, string> = {};
        if (src) attrs.src = src;
        if (alt) attrs.alt = alt;
        if (title) attrs.title = title;
        return ["img", attrs];
      },
    },
    table: {
      content: "table_caption? table_head? table_body table_foot?",
      group: "block",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "table" }],
      toDOM() {
        return ["table", 0];
      },
    },
    table_caption: {
      content: "inline*",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "caption" }],
      toDOM() {
        return ["caption", 0];
      },
    },
    table_head: {
      content: "table_row+",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "thead" }],
      toDOM() {
        return ["thead", 0];
      },
    },
    table_body: {
      content: "table_row+",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "tbody" }],
      toDOM() {
        return ["tbody", 0];
      },
    },
    table_foot: {
      content: "table_row+",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "tfoot" }],
      toDOM() {
        return ["tfoot", 0];
      },
    },
    table_row: {
      content: "(table_cell | table_header)+",
      attrs: sourceRangeAttrs(),
      parseDOM: [{ tag: "tr" }],
      toDOM() {
        return ["tr", 0];
      },
    },
    table_cell: {
      content: "inline*",
      attrs: {
        colspan: { default: 1 },
        rowspan: { default: 1 },
        align: { default: null },
        ...sourceRangeAttrs(),
      },
      parseDOM: [
        {
          tag: "td",
          getAttrs: (dom: Element) => ({
            align: dom.getAttribute("align") ?? (dom as HTMLElement).style?.textAlign ?? null,
          }),
        },
      ],
      toDOM(node) {
        const attrs: Record<string, string> = {};
        if (node.attrs.colspan && node.attrs.colspan !== 1)
          attrs.colspan = String(node.attrs.colspan);
        if (node.attrs.rowspan && node.attrs.rowspan !== 1)
          attrs.rowspan = String(node.attrs.rowspan);
        return ["td", attrs, 0];
      },
    },
    table_header: {
      content: "inline*",
      attrs: {
        colspan: { default: 1 },
        rowspan: { default: 1 },
        align: { default: null },
        ...sourceRangeAttrs(),
      },
      parseDOM: [
        {
          tag: "th",
          getAttrs: (dom: Element) => ({
            align: dom.getAttribute("align") ?? (dom as HTMLElement).style?.textAlign ?? null,
          }),
        },
      ],
      toDOM(node) {
        const attrs: Record<string, string> = {};
        if (node.attrs.colspan && node.attrs.colspan !== 1)
          attrs.colspan = String(node.attrs.colspan);
        if (node.attrs.rowspan && node.attrs.rowspan !== 1)
          attrs.rowspan = String(node.attrs.rowspan);
        return ["th", attrs, 0];
      },
    },
    raw_html_island: {
      group: "block",
      atom: true,
      selectable: true,
      attrs: {
        raw: { default: "" },
        reason: { default: "locked" },
        sourceFrom: { default: -1 },
        sourceTo: { default: -1 },
      },
      parseDOM: [{ tag: "div[data-vr-raw-island]" }],
      toDOM(node) {
        return [
          "div",
          { "data-vr-raw-island": node.attrs.reason, "data-vr-raw": node.attrs.raw.slice(0, 120) },
          `Locked: ${node.attrs.reason}`,
        ];
      },
    },
    text: {
      group: "inline",
    },
  },
  marks: {
    strong: {
      parseDOM: [
        { tag: "strong" },
        {
          tag: "b",
          getAttrs: (node: HTMLElement | string) =>
            typeof node !== "string" && node.style.fontWeight !== "normal" ? null : false,
        },
      ],
      toDOM() {
        return ["strong", 0];
      },
    },
    em: {
      parseDOM: [{ tag: "em" }, { tag: "i" }],
      toDOM() {
        return ["em", 0];
      },
    },
    underline: {
      parseDOM: [{ tag: "u" }],
      toDOM() {
        return ["u", 0];
      },
    },
    strike: {
      parseDOM: [{ tag: "s" }, { tag: "strike" }, { tag: "del" }],
      toDOM() {
        return ["s", 0];
      },
    },
    code: {
      parseDOM: [{ tag: "code" }],
      toDOM() {
        return ["code", 0];
      },
    },
    link: {
      attrs: {
        href: { default: "" },
        title: { default: "" },
      },
      inclusive: false,
      parseDOM: [
        {
          tag: "a[href]",
          getAttrs(dom: HTMLElement | string) {
            if (typeof dom === "string") return false;
            return { href: dom.getAttribute("href") ?? "", title: dom.getAttribute("title") ?? "" };
          },
        },
      ],
      toDOM(mark) {
        const attrs: Record<string, string> = { href: mark.attrs.href };
        if (mark.attrs.title) attrs.title = mark.attrs.title;
        return ["a", attrs, 0];
      },
    },
    textStyle: {
      attrs: {
        fontFamily: { default: null },
        fontSize: { default: null },
        color: { default: null },
        backgroundColor: { default: null },
      },
      parseDOM: [
        {
          tag: "span[style]",
          getAttrs(dom: Element) {
            const attrs = parseHtmlTextStyle(dom.getAttribute("style"));
            return hasHtmlTextStyle(attrs) ? attrs : false;
          },
        },
      ],
      toDOM(mark) {
        return ["span", { style: htmlTextStyleCss(mark.attrs) }, 0];
      },
    },
  },
});

function domTextContent(el: Element): string {
  return el.textContent ?? "";
}

/**
 * Parse sanitized body inner HTML into a ProseMirror doc.
 * Sanitizes via htmlVisualSanitize before DOM creation, never fetches remote.
 */
export function parseHtmlToPm(
  bodyInnerHtml: string,
  revision = 0,
  map?: FragmentSourceMap,
): PmNode {
  const sanitized = sanitizeHtmlForVisual(projectHtmlForVisual(bodyInnerHtml, map));
  // Build PM doc via DOM walk. Empty sanitized -> single empty paragraph
  if (!sanitized.trim()) {
    return htmlVisualSchema.nodes.doc.create({ revision, sourceFrom: -1, sourceTo: -1 }, [
      htmlVisualSchema.nodes.paragraph.create(),
    ]);
  }
  // DOMParser may not be available in some test envs; fallback to template
  let body: HTMLElement;
  if (typeof DOMParser !== "undefined") {
    const doc = new DOMParser().parseFromString(`<body>${sanitized}</body>`, "text/html");
    body = doc.body;
  } else if (typeof document !== "undefined") {
    const div = document.createElement("div");
    div.innerHTML = sanitized;
    body = div as unknown as HTMLElement;
  } else {
    // SSR fallback: return empty doc
    return htmlVisualSchema.nodes.doc.create({ revision }, [
      htmlVisualSchema.nodes.paragraph.create(),
    ]);
  }
  const blocks: PmNode[] = [];
  for (const child of Array.from(body.childNodes)) {
    const pmNode = domNodeToPm(child);
    if (pmNode) {
      if (Array.isArray(pmNode)) blocks.push(...pmNode);
      else blocks.push(pmNode);
    }
  }
  if (blocks.length === 0) blocks.push(htmlVisualSchema.nodes.paragraph.create());
  return htmlVisualSchema.nodes.doc.create({ revision, sourceFrom: -1, sourceTo: -1 }, blocks);
}

function domNodeToPm(dom: ChildNode): PmNode | PmNode[] | null {
  if (dom.nodeType === 3) {
    // Text
    const text = (dom as Text).nodeValue ?? "";
    if (!text.trim()) return null;
    return htmlVisualSchema.nodes.paragraph.create(null, [htmlVisualSchema.text(text)]);
  }
  if (dom.nodeType === 8) return null; // comment ignored in visual projection (preserved in shell/source but not visual)
  if (dom.nodeType !== 1) return null;
  const el = dom as Element;
  const tag = el.tagName.toLowerCase();

  if (el.hasAttribute("data-vr-raw-island")) {
    return htmlVisualSchema.nodes.raw_html_island.create({
      raw: (() => {
        const encoded = el.getAttribute("data-vr-raw") ?? "";
        try {
          return decodeURIComponent(encoded);
        } catch {
          return encoded;
        }
      })(),
      reason: el.getAttribute("data-vr-raw-island") ?? "locked",
      sourceFrom: Number(el.getAttribute("data-vr-raw-start") ?? -1),
      sourceTo: Number(el.getAttribute("data-vr-raw-end") ?? -1),
    });
  }

  const parseInlineChildren = (parent: Element): PmNode[] => {
    const out: PmNode[] = [];
    for (const c of Array.from(parent.childNodes)) {
      const n = domInlineToPm(c);
      if (n) {
        if (Array.isArray(n)) out.push(...n);
        else out.push(n);
      }
    }
    return out;
  };

  const parseBlockChildren = (parent: Element): PmNode[] => {
    const out: PmNode[] = [];
    for (const c of Array.from(parent.childNodes)) {
      const n = domNodeToPm(c);
      if (n) {
        if (Array.isArray(n)) out.push(...n);
        else out.push(n);
      }
    }
    return out;
  };

  switch (tag) {
    case "h1":
    case "h2":
    case "h3":
    case "h4":
    case "h5":
    case "h6": {
      const level = Number(tag[1]);
      const inlines = parseInlineChildren(el);
      return htmlVisualSchema.nodes.heading.create(
        { level },
        inlines.length ? inlines : [htmlVisualSchema.text("")],
      );
    }
    case "p": {
      const inlines = parseInlineChildren(el);
      return htmlVisualSchema.nodes.paragraph.create(null, inlines.length ? inlines : []);
    }
    case "blockquote": {
      const blocks = parseBlockChildren(el);
      return htmlVisualSchema.nodes.blockquote.create(
        null,
        blocks.length ? blocks : [htmlVisualSchema.nodes.paragraph.create()],
      );
    }
    case "article": {
      const blocks = parseBlockChildren(el);
      return htmlVisualSchema.nodes.article.create(
        null,
        blocks.length ? blocks : [htmlVisualSchema.nodes.paragraph.create()],
      );
    }
    case "section": {
      const blocks = parseBlockChildren(el);
      return htmlVisualSchema.nodes.section.create(
        null,
        blocks.length ? blocks : [htmlVisualSchema.nodes.paragraph.create()],
      );
    }
    case "div": {
      const blocks = parseBlockChildren(el);
      // div may contain inline content directly (e.g., <div>text</div>) — wrap into paragraph
      if (blocks.length === 0) {
        const inlines = parseInlineChildren(el);
        if (inlines.length) return htmlVisualSchema.nodes.paragraph.create(null, inlines);
      }
      return htmlVisualSchema.nodes.html_div.create(
        null,
        blocks.length ? blocks : [htmlVisualSchema.nodes.paragraph.create()],
      );
    }
    case "span": {
      const inlines = parseInlineChildren(el);
      if (inlines.length === 0) return null;
      const attrs = parseHtmlTextStyle(el.getAttribute("style"));
      const styleMark = hasHtmlTextStyle(attrs)
        ? htmlVisualSchema.marks.textStyle.create(attrs)
        : null;
      const styled = styleMark
        ? inlines.map((node) => (node.isInline ? node.mark([...node.marks, styleMark]) : node))
        : inlines;
      const otherAttributes = Array.from(el.attributes).some(
        (attribute) => attribute.name.toLowerCase() !== "style",
      );
      return hasHtmlTextStyle(attrs) && !otherAttributes
        ? styled
        : htmlVisualSchema.nodes.html_span.create(null, styled);
    }
    case "ul": {
      const items: PmNode[] = [];
      for (const c of Array.from(el.children)) {
        if (c.tagName.toLowerCase() === "li") {
          const liBlocks = parseBlockChildren(c);
          // Ensure first child is paragraph
          if (liBlocks.length === 0) liBlocks.push(htmlVisualSchema.nodes.paragraph.create());
          items.push(htmlVisualSchema.nodes.list_item.create(null, liBlocks));
        }
      }
      return htmlVisualSchema.nodes.bullet_list.create(
        null,
        items.length
          ? items
          : [
              htmlVisualSchema.nodes.list_item.create(null, [
                htmlVisualSchema.nodes.paragraph.create(),
              ]),
            ],
      );
    }
    case "ol": {
      const startAttr = el.getAttribute("start");
      const start = startAttr ? parseInt(startAttr, 10) || 1 : 1;
      const items: PmNode[] = [];
      for (const c of Array.from(el.children)) {
        if (c.tagName.toLowerCase() === "li") {
          const liBlocks = parseBlockChildren(c);
          if (liBlocks.length === 0) liBlocks.push(htmlVisualSchema.nodes.paragraph.create());
          items.push(htmlVisualSchema.nodes.list_item.create(null, liBlocks));
        }
      }
      return htmlVisualSchema.nodes.ordered_list.create(
        { start },
        items.length
          ? items
          : [
              htmlVisualSchema.nodes.list_item.create(null, [
                htmlVisualSchema.nodes.paragraph.create(),
              ]),
            ],
      );
    }
    case "li": {
      // Fallback when li encountered outside ul/ol (should not happen). Wrap.
      const blocks = parseBlockChildren(el);
      return htmlVisualSchema.nodes.list_item.create(
        null,
        blocks.length ? blocks : [htmlVisualSchema.nodes.paragraph.create()],
      );
    }
    case "pre": {
      // pre may contain code; take textContent
      const text = domTextContent(el);
      // If contains <code> child, use its text
      const codeEl = el.querySelector("code");
      const codeText = codeEl ? (codeEl.textContent ?? text) : text;
      return htmlVisualSchema.nodes.code_block.create(
        null,
        codeText ? htmlVisualSchema.text(codeText) : [],
      );
    }
    case "code": {
      // Inline code if inside paragraph, else block? For simplicity, if parent is pre we already handled, else inline.
      // This domNodeToPm path is for block-level code element not inside inline context.
      // Treat as inline if we are at block level we convert to paragraph with code mark? Simpler: return inline code mark via parent inline parser.
      // But for block-level stray <code>, wrap in paragraph.
      const inlines = parseInlineChildren(el);
      // If we are being called from block context, we need a block. Return paragraph containing code-inlined text.
      // Detect if caller expects block: we are inside domNodeToPm block switch; returning a paragraph is okay.
      // So wrap inlines that carry code mark? Instead use inline conversion.
      // Simpler: return paragraph with code marks.
      const withMark = inlines.map((n) =>
        n.isText ? n.mark([htmlVisualSchema.marks.code.create()]) : n,
      );
      return htmlVisualSchema.nodes.paragraph.create(
        null,
        withMark.length ? withMark : [htmlVisualSchema.text("")],
      );
    }
    case "hr": {
      return htmlVisualSchema.nodes.horizontal_rule.create();
    }
    case "br": {
      return htmlVisualSchema.nodes.hard_break.create();
    }
    case "table": {
      return parseTableElement(el);
    }
    case "img": {
      const src = el.getAttribute("src") ?? "";
      const alt = el.getAttribute("alt") ?? "";
      const title = el.getAttribute("title") ?? "";
      return htmlVisualSchema.nodes.image.create({ src, alt, title });
    }
    case "a": {
      // Anchor at block level: convert to paragraph with link marks
      const href = el.getAttribute("href") ?? "";
      const title = el.getAttribute("title") ?? "";
      const inlines = parseInlineChildren(el);
      const linkMark = htmlVisualSchema.marks.link.create({ href, title });
      const marked = inlines.map((n) => (n.isInline ? n.mark([...n.marks, linkMark]) : n));
      return htmlVisualSchema.nodes.paragraph.create(
        null,
        marked.length ? marked : [htmlVisualSchema.text(href || title || "").mark([linkMark])],
      );
    }
    case "caption":
    case "thead":
    case "tbody":
    case "tfoot":
    case "tr":
    case "th":
    case "td": {
      // These should only appear inside table parser; if encountered stray, wrap in paragraph
      const inlines = parseInlineChildren(el);
      return htmlVisualSchema.nodes.paragraph.create(null, inlines);
    }
    default: {
      // Unknown but we already sanitized; fallback to its children
      const blocks = parseBlockChildren(el);
      if (blocks.length) return blocks;
      const inlines = parseInlineChildren(el);
      if (inlines.length) return htmlVisualSchema.nodes.paragraph.create(null, inlines);
      return null;
    }
  }
}

function domInlineToPm(dom: ChildNode): PmNode | PmNode[] | null {
  if (dom.nodeType === 3) {
    const text = (dom as Text).nodeValue ?? "";
    if (!text) return null;
    return htmlVisualSchema.text(text);
  }
  if (dom.nodeType !== 1) return null;
  const el = dom as Element;
  const tag = el.tagName.toLowerCase();
  const children: PmNode[] = [];
  for (const c of Array.from(el.childNodes)) {
    const n = domInlineToPm(c);
    if (n) {
      if (Array.isArray(n)) children.push(...n);
      else children.push(n);
    }
  }
  switch (tag) {
    case "strong":
    case "b": {
      const mark = htmlVisualSchema.marks.strong.create();
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "em":
    case "i": {
      const mark = htmlVisualSchema.marks.em.create();
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "u": {
      const mark = htmlVisualSchema.marks.underline.create();
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "s": {
      const mark = htmlVisualSchema.marks.strike.create();
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "code": {
      const mark = htmlVisualSchema.marks.code.create();
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "a": {
      const href = el.getAttribute("href") ?? "";
      const title = el.getAttribute("title") ?? "";
      const mark = htmlVisualSchema.marks.link.create({ href, title });
      if (children.length === 0) return htmlVisualSchema.text(href || "").mark([mark]);
      return children.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "span": {
      if (children.length === 0) return null;
      const attrs = parseHtmlTextStyle(el.getAttribute("style"));
      const styled = hasHtmlTextStyle(attrs)
        ? children.map((n) =>
            n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.textStyle.create(attrs)]) : n,
          )
        : children;
      // Style-only spans map directly to one mark. Attribute-bearing spans retain their wrapper;
      // source surgery preserves those authored attributes on Done.
      const otherAttributes = Array.from(el.attributes).some(
        (attribute) => attribute.name.toLowerCase() !== "style",
      );
      return hasHtmlTextStyle(attrs) && !otherAttributes
        ? styled
        : htmlVisualSchema.nodes.html_span.create(null, styled);
    }
    case "img": {
      const src = el.getAttribute("src") ?? "";
      const alt = el.getAttribute("alt") ?? "";
      const title = el.getAttribute("title") ?? "";
      return htmlVisualSchema.nodes.image.create({ src, alt, title });
    }
    case "br": {
      return htmlVisualSchema.nodes.hard_break.create();
    }
    case "hr": {
      // hr inside inline? treat as hard break
      return htmlVisualSchema.nodes.hard_break.create();
    }
    default: {
      // For unknown inline-like, return its children
      return children;
    }
  }
}

function parseTableElement(tableEl: Element): PmNode {
  const captionEl = Array.from(tableEl.children).find((e) => e.tagName.toLowerCase() === "caption");
  const theadEl = Array.from(tableEl.children).find((e) => e.tagName.toLowerCase() === "thead");
  const tbodyEls = Array.from(tableEl.children).filter((e) => e.tagName.toLowerCase() === "tbody");
  const tfootEl = Array.from(tableEl.children).find((e) => e.tagName.toLowerCase() === "tfoot");
  // If no thead/tbody/tfoot, treat direct tr children as tbody
  const directRows = Array.from(tableEl.children).filter((e) => e.tagName.toLowerCase() === "tr");

  const children: PmNode[] = [];

  if (captionEl) {
    const inlines: PmNode[] = [];
    for (const c of Array.from(captionEl.childNodes)) {
      const n = domInlineChildrenToPm(c);
      if (n) {
        if (Array.isArray(n)) inlines.push(...n);
        else inlines.push(n);
      }
    }
    children.push(htmlVisualSchema.nodes.table_caption.create(null, inlines));
  }

  const buildRows = (container: Element | null, fallbackRows: Element[]): PmNode[] => {
    const rowEls = container
      ? Array.from(container.children).filter((e) => e.tagName.toLowerCase() === "tr")
      : fallbackRows;
    return rowEls.map((tr) => {
      const cells: PmNode[] = [];
      for (const cell of Array.from(tr.children)) {
        const t = cell.tagName.toLowerCase();
        if (t !== "td" && t !== "th") continue;
        const inlines: PmNode[] = [];
        for (const c of Array.from(cell.childNodes)) {
          const n = domInlineChildrenToPm(c);
          if (n) {
            if (Array.isArray(n)) inlines.push(...n);
            else inlines.push(n);
          }
        }
        const colspan = cell.getAttribute("colspan")
          ? Number(cell.getAttribute("colspan")) || 1
          : 1;
        const rowspan = cell.getAttribute("rowspan")
          ? Number(cell.getAttribute("rowspan")) || 1
          : 1;
        const align = cell.getAttribute("align") ?? (cell as HTMLElement).style?.textAlign ?? null;
        if (t === "th")
          cells.push(
            htmlVisualSchema.nodes.table_header.create({ colspan, rowspan, align }, inlines),
          );
        else
          cells.push(
            htmlVisualSchema.nodes.table_cell.create({ colspan, rowspan, align }, inlines),
          );
      }
      return htmlVisualSchema.nodes.table_row.create(
        null,
        cells.length ? cells : [htmlVisualSchema.nodes.table_cell.create()],
      );
    });
  };

  if (theadEl) {
    const rows = buildRows(theadEl, []);
    if (rows.length) children.push(htmlVisualSchema.nodes.table_head.create(null, rows));
  }
  if (tbodyEls.length) {
    for (const tb of tbodyEls) {
      const rows = buildRows(tb, []);
      if (rows.length) children.push(htmlVisualSchema.nodes.table_body.create(null, rows));
    }
  } else if (directRows.length) {
    const rows = buildRows(null, directRows);
    if (rows.length) children.push(htmlVisualSchema.nodes.table_body.create(null, rows));
  } else if (!theadEl && !tfootEl && children.length === 0) {
    // Empty table -> single body with empty row
    children.push(
      htmlVisualSchema.nodes.table_body.create(null, [
        htmlVisualSchema.nodes.table_row.create(null, [htmlVisualSchema.nodes.table_cell.create()]),
      ]),
    );
  }
  if (tfootEl) {
    const rows = buildRows(tfootEl, []);
    if (rows.length) children.push(htmlVisualSchema.nodes.table_foot.create(null, rows));
  }

  return htmlVisualSchema.nodes.table.create(null, children);
}

function domInlineChildrenToPm(node: ChildNode): PmNode | PmNode[] | null {
  if (node.nodeType === 3) {
    const t = (node as Text).nodeValue ?? "";
    if (!t) return null;
    return htmlVisualSchema.text(t);
  }
  if (node.nodeType !== 1) return null;
  const el = node as Element;
  const tag = el.tagName.toLowerCase();
  const inner: PmNode[] = [];
  for (const c of Array.from(el.childNodes)) {
    const n = domInlineChildrenToPm(c);
    if (n) {
      if (Array.isArray(n)) inner.push(...n);
      else inner.push(n);
    }
  }
  switch (tag) {
    case "strong":
    case "b":
      return inner.map((n) =>
        n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.strong.create()]) : n,
      );
    case "em":
    case "i":
      return inner.map((n) =>
        n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.em.create()]) : n,
      );
    case "u":
      return inner.map((n) =>
        n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.underline.create()]) : n,
      );
    case "s":
      return inner.map((n) =>
        n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.strike.create()]) : n,
      );
    case "code":
      return inner.map((n) =>
        n.isInline ? n.mark([...n.marks, htmlVisualSchema.marks.code.create()]) : n,
      );
    case "a": {
      const href = el.getAttribute("href") ?? "";
      const title = el.getAttribute("title") ?? "";
      const mark = htmlVisualSchema.marks.link.create({ href, title });
      if (inner.length === 0) return htmlVisualSchema.text(href).mark([mark]);
      return inner.map((n) => (n.isInline ? n.mark([...n.marks, mark]) : n));
    }
    case "span": {
      const attrs = parseHtmlTextStyle(el.getAttribute("style"));
      const styleMark = hasHtmlTextStyle(attrs)
        ? htmlVisualSchema.marks.textStyle.create(attrs)
        : null;
      const styled = styleMark
        ? inner.map((child) => (child.isInline ? child.mark([...child.marks, styleMark]) : child))
        : inner;
      const otherAttributes = Array.from(el.attributes).some(
        (attribute) => attribute.name.toLowerCase() !== "style",
      );
      return hasHtmlTextStyle(attrs) && !otherAttributes
        ? styled
        : htmlVisualSchema.nodes.html_span.create(null, styled);
    }
    case "img": {
      const src = el.getAttribute("src") ?? "";
      const alt = el.getAttribute("alt") ?? "";
      const title = el.getAttribute("title") ?? "";
      return htmlVisualSchema.nodes.image.create({ src, alt, title });
    }
    case "br":
      return htmlVisualSchema.nodes.hard_break.create();
    default:
      return inner;
  }
}
