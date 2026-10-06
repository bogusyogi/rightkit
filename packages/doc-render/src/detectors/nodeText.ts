import type { RootContent } from "mdast";

/** Flatten the visible text of an mdast node (headings, paragraphs, list items, etc.). */
export function nodeText(node: RootContent | undefined): string {
  if (!node) return "";
  const anyNode = node as { value?: string; children?: RootContent[] };
  if (typeof anyNode.value === "string") return anyNode.value;
  if (Array.isArray(anyNode.children)) return anyNode.children.map((c) => nodeText(c)).join("");
  return "";
}
