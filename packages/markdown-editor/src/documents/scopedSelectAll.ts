export function isSelectAllShortcut(
  event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey">,
): boolean {
  return (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "a";
}

export function isEditableSelectionTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  const tag = target.tagName.toLowerCase();
  if (tag === "input" || tag === "textarea" || tag === "select") return true;
  if (target instanceof HTMLElement && typeof target.isContentEditable === "boolean") {
    return target.isContentEditable;
  }
  const editable = target.closest("[contenteditable]");
  const value = editable?.getAttribute("contenteditable")?.trim().toLowerCase();
  return value === "" || value === "true" || value === "plaintext-only";
}

export function selectionIntersectsElement(element: Element, win: Window = window): boolean {
  const selection = win.getSelection();
  if (!selection || selection.rangeCount === 0) return false;
  for (let i = 0; i < selection.rangeCount; i += 1) {
    const range = selection.getRangeAt(i);
    if (range.intersectsNode(element)) return true;
  }
  return false;
}

export function selectElementContents(element: Element, win: Window = window) {
  const doc = element.ownerDocument;
  const range = doc.createRange();
  range.selectNodeContents(element);
  const selection = win.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

type CaretDocument = Document & {
  caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  caretRangeFromPoint?: (x: number, y: number) => Range | null;
};

function caretRangeAtPoint(doc: CaretDocument, x: number, y: number): Range | null {
  const position = doc.caretPositionFromPoint?.(x, y);
  if (position) {
    const range = doc.createRange();
    range.setStart(position.offsetNode, position.offset);
    range.collapse(true);
    return range;
  }
  return doc.caretRangeFromPoint?.(x, y) ?? null;
}

export function selectVisualLineAtPoint(element: HTMLElement, clientY: number): boolean {
  const doc = element.ownerDocument as CaretDocument;
  const rect = element.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  const xStart = Math.min(rect.right - 1, rect.left + 1);
  const xEnd = Math.max(xStart, rect.right - 1);
  const y = Math.max(rect.top + 1, Math.min(rect.bottom - 1, clientY));
  const start = caretRangeAtPoint(doc, xStart, y);
  const end = caretRangeAtPoint(doc, xEnd, y);
  if (
    !start ||
    !end ||
    !element.contains(start.startContainer) ||
    !element.contains(end.startContainer)
  ) {
    return false;
  }

  const range = doc.createRange();
  if (start.compareBoundaryPoints(Range.START_TO_START, end) <= 0) {
    range.setStart(start.startContainer, start.startOffset);
    range.setEnd(end.startContainer, end.startOffset);
  } else {
    range.setStart(end.startContainer, end.startOffset);
    range.setEnd(start.startContainer, start.startOffset);
  }
  const selection = doc.defaultView?.getSelection();
  if (!selection) return false;
  selection.removeAllRanges();
  selection.addRange(range);
  return true;
}
