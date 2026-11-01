import { HTML_INLINE_EDITABLE_SELECTOR, type HtmlInspectTarget } from "./sourceMap.js";

/**
 * WKWebView suppresses callbacks registered inside a sandbox that intentionally omits
 * `allow-scripts`. Native double-click still creates a same-origin word selection, so Safe mode
 * can recognize that gesture from parent without weakening its scriptless sandbox.
 */
export function webKitSelectedWordTarget(selection: Selection | null): Element | null {
  if (!selection || selection.isCollapsed || selection.anchorNode !== selection.focusNode)
    return null;
  const selectedText = selection.toString().trim();
  if (!selectedText || /\s/.test(selectedText) || selectedText.length > 180) return null;
  const node = selection.anchorNode;
  const element = node instanceof Element ? node : node?.parentElement;
  // A word selected inside the open block editor is an editing gesture, not a new edit target.
  if (!element || isInsideBlockEditor(element)) return null;
  return element.closest(HTML_INLINE_EDITABLE_SELECTOR) ?? null;
}

/**
 * True when `element` lies inside the in-frame ProseMirror block editor (its mount carries the
 * `ProseMirror` class). That subtree is editor-owned scaffold — block targets such as `<li>` render
 * their own `ul > li > p` inside the authored element — so its child paths never map to source and
 * gestures there belong to the editor rather than to Safe-mode block selection.
 */
export function isInsideBlockEditor(element: Element): boolean {
  return element.closest(".ProseMirror") !== null;
}

export function clampPreviewZoom(value: number): number {
  return Math.max(0.5, Math.min(3, Math.round(value * 100) / 100));
}

function normalizedInspectText(value: string | null | undefined): string {
  return (value ?? "").replace(/\s+/g, " ").trim();
}

export function inspectTargetFromClick(
  doc: Document,
  win: Window | null,
  event: MouseEvent,
): HtmlInspectTarget | null {
  const selectedText = normalizedInspectText(win?.getSelection?.()?.toString()).slice(0, 180);
  let targetElement: Element | null = null;
  try {
    targetElement = doc.elementFromPoint?.(event.clientX, event.clientY) ?? null;
  } catch (_) {
    targetElement = null;
  }
  targetElement = targetElement ?? (event.target instanceof Element ? event.target : null);
  let node: Node | null = null;
  try {
    const range = (
      doc as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null }
    ).caretRangeFromPoint?.(event.clientX, event.clientY);
    node = range?.startContainer ?? null;
  } catch (_) {
    node = null;
  }
  if (node && targetElement && !targetElement.contains(node)) node = null;
  if (!node || node.nodeType !== Node.TEXT_NODE) {
    const root = targetElement ?? doc.body;
    const filter =
      (win as (Window & { NodeFilter?: typeof NodeFilter }) | null)?.NodeFilter ?? NodeFilter;
    const walker = doc.createTreeWalker(root, filter.SHOW_TEXT, {
      acceptNode(n) {
        return normalizedInspectText(n.nodeValue) ? filter.FILTER_ACCEPT : filter.FILTER_REJECT;
      },
    });
    node = walker.nextNode();
  }
  const text = normalizedInspectText(node?.nodeValue).slice(0, 180);
  // Anchor/image identity comes from the geometric hit element (targetElement), never from the
  // text node's parent: the text node is only a best-effort preview string and can land in a
  // different anchor than the one actually under the pointer (e.g. a block with multiple links),
  // or be absent entirely (an image-only block has no text node at all).
  const anchor = targetElement?.closest?.("a[href]") as HTMLAnchorElement | null | undefined;
  const image = targetElement?.closest?.("img[src]") as HTMLImageElement | null | undefined;
  if (!text && !anchor && !image) return null;
  const parent = node?.parentElement ?? targetElement?.closest("*") ?? null;
  return {
    text,
    selectedText: selectedText || undefined,
    tagName: parent?.tagName.toLowerCase(),
    id: parent?.id || undefined,
    classNames: parent ? Array.from(parent.classList).slice(0, 8) : [],
    href: anchor?.getAttribute("href") || undefined,
    src: image?.getAttribute("src") || undefined,
  };
}
