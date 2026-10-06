import type { Heading, List, ListItem } from "mdast";
import type { Detector, DetectorHit } from "./types";
import { nodeText } from "./nodeText.js";

export interface ComparisonData {
  left: { title: string; items: string[] };
  right: { title: string; items: string[] };
  /** "pro-con" tints the cards; "neutral" renders them evenly. */
  tone: "pro-con" | "neutral";
}

const POSITIVE = /\b(pros?|advantages?|benefits?|upsides?|for)\b/i;
const NEGATIVE = /\b(cons?|disadvantages?|drawbacks?|downsides?|against|risks?)\b/i;

function listItems(node: List): string[] {
  return node.children
    .filter((c): c is ListItem => c.type === "listItem")
    .map((li) => nodeText(li).trim())
    .filter(Boolean);
}

/** Comparison: two consecutive `heading + bullet list` pairs (e.g. Pros / Cons). */
export const comparisonDetector: Detector = (nodes, index, hint): DetectorHit | null => {
  const [h1, l1, h2, l2] = [nodes[index], nodes[index + 1], nodes[index + 2], nodes[index + 3]];
  if (
    h1?.type !== "heading" ||
    l1?.type !== "list" ||
    h2?.type !== "heading" ||
    l2?.type !== "list"
  )
    return null;
  if ((l1 as List).ordered || (l2 as List).ordered) return null;

  const leftTitle = nodeText(h1 as Heading).trim();
  const rightTitle = nodeText(h2 as Heading).trim();
  const data: ComparisonData = {
    left: { title: leftTitle, items: listItems(l1 as List) },
    right: { title: rightTitle, items: listItems(l2 as List) },
    tone: "neutral",
  };
  if (data.left.items.length === 0 || data.right.items.length === 0) return null;

  if (hint === "comparison")
    return { kind: "comparison", confidence: 1, consumed: 4, data, forced: true };

  const proCon = POSITIVE.test(leftTitle) && NEGATIVE.test(rightTitle);
  if (proCon) data.tone = "pro-con";

  // Same heading depth + a recognized antonym pair is a strong signal.
  const sameDepth = (h1 as Heading).depth === (h2 as Heading).depth;
  const confidence = proCon && sameDepth ? 0.85 : sameDepth ? 0.4 : 0.25;

  return { kind: "comparison", confidence, consumed: 4, data };
};
