import { TextSelection } from "prosemirror-state";
import type { EditorView } from "prosemirror-view";
import { selectElementContents, selectVisualLineAtPoint } from "@rightkit/document-core/documents/scopedSelectAll";
import type { TextNodeMapping } from "@rightkit/document-core/documents/searchUtils";
import { getVisibleTextNodes } from "@rightkit/document-core/documents/searchUtils";
import { markdownToPmDoc } from "./projection/pmFromMarkdown.js";

export function selectBlockFromLeftEdge(
  root: HTMLElement,
  event: MouseEvent,
  gutterRoot: HTMLElement = root,
): boolean {
  if (event.button !== 0 || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey)
    return false;
  const target = event.target as HTMLElement | null;
  if (!target || target.closest("a,button,input,select,textarea,[role='button']")) return false;
  const selector = "[data-vr-protected-block],p,h1,h2,h3,h4,h5,h6,blockquote,li,pre";
  const block =
    target.closest<HTMLElement>(selector) ??
    Array.from(root.querySelectorAll<HTMLElement>(selector)).find((candidate) => {
      const rect = candidate.getBoundingClientRect();
      return event.clientY >= rect.top && event.clientY <= rect.bottom;
    });
  if (!block || !root.contains(block)) return false;
  const rect = block.getBoundingClientRect();
  const rootRect = gutterRoot.getBoundingClientRect();
  if (event.clientX < rootRect.left || event.clientX >= rect.left) return false;
  event.preventDefault();
  event.stopPropagation();
  if (!selectVisualLineAtPoint(block, event.clientY)) selectElementContents(block);
  return true;
}

export function replaceWholeDocumentWithMarkdown(view: EditorView, source: string) {
  const nextDoc = markdownToPmDoc(source);
  const transaction = view.state.tr
    .replaceWith(0, view.state.doc.content.size, nextDoc.content)
    .setDocAttribute("trailing", nextDoc.attrs.trailing ?? "");
  view.dispatch(transaction.setSelection(TextSelection.atEnd(transaction.doc)).scrollIntoView());
}

export function searchMappingsFor(root: HTMLElement): {
  text: string;
  mappings: TextNodeMapping[];
} {
  const visible = getVisibleTextNodes(root);
  let text = "";
  const mappings: TextNodeMapping[] = [];
  let previousBlock: Element | null = null;
  for (const mapping of visible) {
    const block = topLevelBlockFor(root, mapping.node);
    if (previousBlock && block && block !== previousBlock) text += "\n";
    const value = mapping.node.nodeValue ?? "";
    mappings.push({ node: mapping.node, start: text.length, end: text.length + value.length });
    text += value;
    previousBlock = block;
  }
  return { text, mappings };
}

function topLevelBlockFor(root: HTMLElement, node: Node): Element | null {
  let current = node.parentElement;
  let candidate: Element | null = null;
  while (current && current !== root) {
    candidate = current;
    current = current.parentElement;
  }
  return candidate;
}
