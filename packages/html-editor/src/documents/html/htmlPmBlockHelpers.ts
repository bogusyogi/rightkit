import type { CSSProperties } from "react";
import { Schema } from "prosemirror-model";
import { type EditorState, Plugin, PluginKey, TextSelection } from "prosemirror-state";
import { toggleMark } from "prosemirror-commands";
import type { EditorView } from "prosemirror-view";
import type { BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import { htmlVisualSchema, parseHtmlToPm } from "./htmlPmParse";
import { serializePmToHtmlFragment } from "./htmlPmSerialize";
import { sanitizeHtmlForVisual } from "./htmlVisualSanitize";
import type { HtmlInlineStyle } from "./sourceMap";
import type { HtmlTextStyleProperty } from "./htmlTextStyle";

// Inline targets already provide their authored outer element. Give their EditorView an inline
// top document so PM does not add a nested same-tag block node. Container targets retain block doc.
const inlineNodes = htmlVisualSchema.spec.nodes.update("doc", {
  ...htmlVisualSchema.spec.nodes.get("doc"),
  content: "inline*",
});
export const htmlInlineSchema = new Schema({
  nodes: inlineNodes,
  marks: htmlVisualSchema.spec.marks,
});

export function isInlineTargetTag(tag: string, inner?: string): boolean {
  if (tag === "p" || tag === "span" || /^h[1-6]$/.test(tag)) return true;
  if ((tag !== "td" && tag !== "th") || inner === undefined) return false;
  // Plain/inline-authored cells supply their own text container. Cells containing blocks or
  // nested tables keep the block schema, preserving those authored child structures.
  const doc = new DOMParser().parseFromString(inner, "text/html");
  const inlineTags = new Set([
    "a",
    "abbr",
    "b",
    "bdi",
    "bdo",
    "br",
    "cite",
    "code",
    "del",
    "em",
    "i",
    "img",
    "ins",
    "kbd",
    "mark",
    "q",
    "s",
    "samp",
    "small",
    "span",
    "strong",
    "sub",
    "sup",
    "time",
    "u",
    "var",
    "wbr",
  ]);
  return [...doc.body.querySelectorAll("*")].every((element) =>
    inlineTags.has(element.tagName.toLowerCase()),
  );
}

export function restoreTargetSnapshot(
  target: HTMLElement,
  original: { tagName: string; attributes: Array<[string, string]>; innerHTML: string },
): HTMLElement {
  const restored =
    target.tagName.toLowerCase() === original.tagName
      ? target
      : target.ownerDocument.createElement(original.tagName);
  for (const attribute of Array.from(restored.attributes)) restored.removeAttribute(attribute.name);
  for (const [name, value] of original.attributes) restored.setAttribute(name, value);
  restored.innerHTML = original.innerHTML;
  if (restored !== target) target.replaceWith(restored);
  return restored;
}

export function parseInframeInline(inner: string, revision: number) {
  const parsed = parseHtmlToPm(`<p>${sanitizeHtmlForVisual(inner)}</p>`, revision);
  const paragraph = parsed.firstChild;
  return htmlInlineSchema.nodeFromJSON({
    type: "doc",
    attrs: { revision, sourceFrom: -1, sourceTo: -1 },
    content: paragraph?.content.toJSON() ?? [],
  });
}

export function serializeInframeInline(doc: ReturnType<typeof parseInframeInline>): string {
  let html = sanitizeHtmlForVisual(serializePmToHtmlFragment(doc));
  // ProseMirror renders overlapping marks as split anchors (e.g. `<a>hello </a><strong><a>world
  // </a></strong>`). Rejoin that transient representation before narrow source surgery so an
  // authored anchor remains one wrapper with its original attributes/quoting supplied by source.
  const splitLink =
    /(<a\b([^>]*)>([\s\S]*?)<\/a>)(<([a-z][\w-]*)>\s*<a\b([^>]*)>([\s\S]*?)<\/a>\s*<\/\5>)/gi;
  const splitLinkWithMark =
    /(<a\b([^>]*)>([\s\S]*?)<\/a>)\s*(<a\b([^>]*)>\s*<([a-z][\w-]*)>([\s\S]*?)<\/\6>\s*<\/a>)/gi;
  let previous = "";
  while (html !== previous) {
    previous = html;
    html = html.replace(
      splitLink,
      (_match, _first, firstAttrs, firstInner, _mark, mark, secondAttrs, secondInner) =>
        firstAttrs.trim() === secondAttrs.trim()
          ? `<a${firstAttrs}>${firstInner}<${mark}>${secondInner}</${mark}></a>`
          : _match,
    );
    html = html.replace(
      splitLinkWithMark,
      (_match, _first, firstAttrs, firstInner, _second, secondAttrs, mark, secondInner) =>
        firstAttrs.trim() === secondAttrs.trim()
          ? `<a${firstAttrs}>${firstInner}<${mark}>${secondInner}</${mark}></a>`
          : _match,
    );
  }
  return html;
}

export function syncSelectionFromIframeDom(view: EditorView): boolean {
  const root = view.dom.getRootNode() as Document | ShadowRoot;
  const rootSelection =
    "getSelection" in root && typeof root.getSelection === "function" ? root.getSelection() : null;
  const documentSelection = view.dom.ownerDocument.getSelection();
  const windowSelection = view.dom.ownerDocument.defaultView?.getSelection() ?? null;
  const selection = [rootSelection, documentSelection, windowSelection].find(
    (candidate) =>
      !!candidate?.anchorNode &&
      !!candidate.focusNode &&
      view.dom.contains(candidate.anchorNode) &&
      view.dom.contains(candidate.focusNode),
  );
  if (!selection?.anchorNode || !selection.focusNode) return false;
  try {
    let anchor: number;
    let head: number;
    try {
      anchor = view.posAtDOM(selection.anchorNode, selection.anchorOffset, 1);
      head = view.posAtDOM(selection.focusNode, selection.focusOffset, -1);
    } catch {
      // WKWebView can expose a native selection whose DOM nodes are visible but not accepted by
      // ProseMirror's DOM-position mapper. Text offsets remain stable across inline mark wrappers.
      const textOffset = (node: Node, offset: number) => {
        const range = view.dom.ownerDocument.createRange();
        range.selectNodeContents(view.dom);
        range.setEnd(node, offset);
        return range.toString().length;
      };
      anchor = positionForTextOffset(
        view.state.doc,
        textOffset(selection.anchorNode, selection.anchorOffset),
      );
      head = positionForTextOffset(
        view.state.doc,
        textOffset(selection.focusNode, selection.focusOffset),
      );
    }
    const next = TextSelection.between(
      view.state.doc.resolve(anchor),
      view.state.doc.resolve(head),
    );
    if (!next.eq(view.state.selection)) {
      view.dispatch(view.state.tr.setSelection(next));
      return true;
    }
  } catch {
    // DOM selection can briefly point at a detached node during iframe/host replacement.
  }
  return false;
}

export function positionForTextOffset(doc: EditorState["doc"], target: number): number {
  let consumed = 0;
  let found: number | null = null;
  doc.descendants((node, pos) => {
    if (found !== null || !node.isText) return found === null;
    const length = node.text?.length ?? 0;
    if (target <= consumed + length) {
      found = pos + Math.max(0, target - consumed);
      return false;
    }
    consumed += length;
    return true;
  });
  return found ?? doc.content.size;
}

export function initialTextSelection(
  state: EditorState,
  bookmark: BlockDraftSnapshot["selectionBookmark"],
): TextSelection | null {
  if (!bookmark || typeof bookmark !== "object") return null;
  const value = bookmark as { kind?: unknown; from?: unknown; to?: unknown };
  if (
    value.kind !== "html-text" ||
    typeof value.from !== "number" ||
    typeof value.to !== "number" ||
    value.from < 0 ||
    value.to <= value.from
  )
    return null;
  const from = positionForTextOffset(state.doc, value.from);
  const to = positionForTextOffset(state.doc, value.to);
  if (from >= to) return null;
  return TextSelection.create(state.doc, from, to);
}

// A page with no authored background paints on the browser canvas, which is white in our isolated
// preview. Falling back to the dark app panel makes ordinary black page text unreadable while it is
// edited. Match the preview canvas instead; authored opaque backgrounds are still preserved below.
const OPAQUE_EDITOR_FALLBACK_BG = "rgb(255, 255, 255)";

export type HtmlEditorToolbarState = {
  canAddLink: boolean;
  marks: Record<
    "strong" | "em" | "code" | "strike" | "underline",
    { active: boolean; enabled: boolean }
  >;
  textStyle: Record<
    HtmlTextStyleProperty,
    { value: string | null; mixed: boolean; enabled: boolean }
  >;
};

export function toolbarStateFor(view: EditorView): HtmlEditorToolbarState {
  const state = view.state;
  const marks = state.storedMarks ?? state.selection.$from.marks();
  const stateForMark = (name: "strong" | "em" | "code" | "strike" | "underline") => {
    const type = state.schema.marks[name];
    return {
      active: !!type?.isInSet(marks),
      enabled: !!type && toggleMark(type)(state),
    };
  };
  const styleType = state.schema.marks.textStyle;
  const stateForStyle = (property: HtmlTextStyleProperty) => {
    const values = new Set<string | null>();
    if (state.selection.empty) {
      const mark = styleType?.isInSet(marks);
      values.add((mark?.attrs[property] as string | null | undefined) ?? null);
    } else {
      state.doc.nodesBetween(state.selection.from, state.selection.to, (node) => {
        if (!node.isText) return;
        const mark = styleType?.isInSet(node.marks);
        values.add((mark?.attrs[property] as string | null | undefined) ?? null);
      });
    }
    return {
      value: values.size === 1 ? ([...values][0] ?? null) : null,
      mixed: values.size > 1,
      enabled: !!styleType,
    };
  };
  return {
    canAddLink: !state.selection.empty,
    marks: {
      strong: stateForMark("strong"),
      em: stateForMark("em"),
      code: stateForMark("code"),
      strike: stateForMark("strike"),
      underline: stateForMark("underline"),
    },
    textStyle: {
      fontFamily: stateForStyle("fontFamily"),
      fontSize: stateForStyle("fontSize"),
      color: stateForStyle("color"),
      backgroundColor: stateForStyle("backgroundColor"),
    },
  };
}

// getComputedStyle().backgroundColor almost never comes back undefined/"" — an element with no
// author background still reports "rgba(0, 0, 0, 0)" (fully transparent), which is truthy as a
// string. Treat that (and "transparent", and any alpha-0 rgba()/hsla()) as absent so the editor
// never renders see-through over the original block text (ghost text).
export function resolveOpaqueEditorBackground(captured: string | undefined): string {
  if (!captured) return OPAQUE_EDITOR_FALLBACK_BG;
  const value = captured.trim().toLowerCase();
  if (value === "" || value === "transparent") return OPAQUE_EDITOR_FALLBACK_BG;
  const alphaMatch = value.match(/^(?:rgba|hsla)\([^)]*,\s*([\d.]+)\s*\)$/);
  if (alphaMatch && Number.parseFloat(alphaMatch[1]) === 0) return OPAQUE_EDITOR_FALLBACK_BG;
  return captured;
}

// Improvement #4 (markdown input rules): typing a markdown block-start marker followed by a
// space transforms the block the same way the block-type dropdown does — a tag rename over the
// same `changeBlockType` path, never a private mutation. Scope is deliberately limited to
// transforms `changeBlockType` already supports (RENAMEABLE_TAGS): heading levels, blockquote,
// and code. List markers are not included: turning a `<p>` into `<ul><li>` is a structural
// reshape (new wrapper + list item), not a tag rename, and the single-block editor has no
// planner op for that yet — forcing it here would mean a private splice outside
// `commitHtmlMutation`, which the source-patch invariant forbids.
const HTML_MARKDOWN_HEADING_RULE = /^(#{1,6})$/;
const HTML_MARKDOWN_BLOCKQUOTE_RULE = /^>$/;
const HTML_MARKDOWN_CODE_FENCE_RULE = /^```$/;

/**
 * Pure trigger-detection for the markdown input rules. `currentTag` gates eligibility (only a
 * plain paragraph transforms; a block that already declares h1-h6/blockquote/pre does not
 * re-trigger), and `textBeforeCaret` is the block-local text already typed before the
 * just-pressed space. Exported standalone so the decision logic is unit-testable without
 * driving a real contentEditable `beforeinput` event through jsdom.
 */
export function resolveMarkdownBlockRule(
  currentTag: string,
  textBeforeCaret: string,
): string | null {
  if (currentTag !== "p") return null;
  const heading = textBeforeCaret.match(HTML_MARKDOWN_HEADING_RULE);
  if (heading) return `h${heading[1].length}`;
  if (HTML_MARKDOWN_BLOCKQUOTE_RULE.test(textBeforeCaret)) return "blockquote";
  if (HTML_MARKDOWN_CODE_FENCE_RULE.test(textBeforeCaret)) return "pre";
  return null;
}

export function markdownBlockRulesPlugin(
  tagRef: { current: string },
  applyBlockTypeRef: { current: (nextTag: string) => void },
) {
  return new Plugin({
    key: new PluginKey("htmlSafeBlockMarkdownRules"),
    props: {
      handleTextInput(view, from, to, text) {
        if (text !== " ") return false;
        const $from = view.state.doc.resolve(from);
        const before = $from.parent.textBetween(0, $from.parentOffset);
        const nextTag = resolveMarkdownBlockRule(tagRef.current, before);
        if (!nextTag) return false;
        const markerStart = from - before.length;
        view.dispatch(view.state.tr.delete(markerStart, to));
        applyBlockTypeRef.current(nextTag);
        return true;
      },
    },
  });
}

export function wrapped(tag: string, inner: string): string {
  // Not sanitized here: parseHtmlToPm projects locked islands from these exact bytes first, then
  // sanitizes. Sanitizing before projection would strip protected <script> and similar subtrees.
  const clean = inner;
  if (tag === "li") return `<ul><li>${clean}</li></ul>`;
  if (tag === "td" || tag === "th")
    return `<table><tbody><tr><${tag}>${clean}</${tag}></tr></tbody></table>`;
  return `<${tag}>${clean}</${tag}>`;
}

/** Map the renameable HTML block tags to their existing visual-schema node types. */
export function visualNodeForTag(tag: string) {
  switch (tag) {
    case "h1":
    case "h2":
    case "h3":
    case "h4":
    case "h5":
    case "h6":
      return htmlVisualSchema.nodes.heading;
    case "blockquote":
      return htmlVisualSchema.nodes.blockquote;
    case "article":
      return htmlVisualSchema.nodes.article;
    case "section":
      return htmlVisualSchema.nodes.section;
    case "div":
      return htmlVisualSchema.nodes.html_div;
    case "pre":
      return htmlVisualSchema.nodes.code_block;
    case "p":
      return htmlVisualSchema.nodes.paragraph;
    default:
      return null;
  }
}

export function visualAttrsForTag(tag: string, current: Record<string, unknown>) {
  if (/^h[1-6]$/.test(tag)) return { ...current, level: Number(tag.slice(1)) };
  return current;
}

export function innerFromDoc(tag: string, doc: ReturnType<typeof parseHtmlToPm>): string {
  const serialized = serializePmToHtmlFragment(doc);
  const islands: string[] = [];
  doc.descendants((node) => {
    if (node.type.name === "raw_html_island") islands.push(String(node.attrs.raw ?? ""));
  });
  // Sanitize everything except raw islands, which are restored byte-for-byte afterwards.
  const sanitizeKeepingIslands = (html: string): string => {
    if (!islands.length) return sanitizeHtmlForVisual(html);
    let guarded = html;
    let cursor = 0;
    for (let i = 0; i < islands.length; i++) {
      const at = islands[i] ? guarded.indexOf(islands[i], cursor) : -1;
      if (at < 0) continue;
      const token = `<div data-vr-raw-island="ph:${i}"></div>`;
      guarded = guarded.slice(0, at) + token + guarded.slice(at + islands[i].length);
      cursor = at + token.length;
    }
    let clean = sanitizeHtmlForVisual(guarded);
    for (let i = 0; i < islands.length; i++) {
      clean = clean.replace(`<div data-vr-raw-island="ph:${i}"></div>`, () => islands[i]);
    }
    return clean;
  };
  let topLevelBlocks = 0;
  doc.forEach(() => {
    topLevelBlocks++;
  });
  if (topLevelBlocks !== 1) return sanitizeKeepingIslands(serialized);
  // Parse in an inert document: raw islands carry untrusted bytes (handlers, scripts), and
  // assigning them to an element of the live document would load images and fire onerror.
  const container = document.implementation.createHTMLDocument("").createElement("div");
  container.innerHTML = serialized;
  const wrapper = container.firstElementChild;
  if (!wrapper) return sanitizeKeepingIslands(serialized);
  const wrapperTag = wrapper.tagName.toLowerCase();
  if (wrapperTag === tag) return wrapper.innerHTML;
  if ((wrapperTag === "ul" || wrapperTag === "ol") && tag === "li") {
    const items = Array.from(wrapper.children).filter((child) => child.tagName === "LI");
    if (items.length === 1) return (items[0] as HTMLElement).innerHTML;
    return wrapper.innerHTML;
  }
  if (wrapperTag === "table" && (tag === "td" || tag === "th")) {
    const row = wrapper.querySelector("tr");
    const cells = row
      ? Array.from(row.children).filter((child) => child.tagName === "TD" || child.tagName === "TH")
      : [];
    if (cells.length === 1) return (cells[0] as HTMLElement).innerHTML;
    return row ? row.innerHTML : sanitizeKeepingIslands(serialized);
  }
  return sanitizeKeepingIslands(serialized);
}

/** Keep a draft-only heading rename visibly meaningful even while canonical HTML stays unchanged. */
export function editorTypographyForTag(
  tag: string,
  originalTag: string,
  visualStyle?: HtmlInlineStyle,
): CSSProperties {
  const headingSizes: Record<string, string> = {
    h1: "2em",
    h2: "1.5em",
    h3: "1.17em",
    h4: "1em",
    h5: "0.83em",
    h6: "0.67em",
  };
  const changedHeading = /^h[1-6]$/.test(tag) && tag !== originalTag;
  return {
    fontFamily: visualStyle?.fontFamily,
    fontSize: changedHeading ? headingSizes[tag] : visualStyle?.fontSize,
    fontWeight: changedHeading ? "bold" : visualStyle?.fontWeight,
    fontStyle: visualStyle?.fontStyle,
    lineHeight: visualStyle?.lineHeight,
    letterSpacing: visualStyle?.letterSpacing,
    color: visualStyle?.color,
    backgroundColor: resolveOpaqueEditorBackground(visualStyle?.backgroundColor),
    textAlign: visualStyle?.textAlign as CSSProperties["textAlign"],
    padding: visualStyle?.padding,
  };
}
