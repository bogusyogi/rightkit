import type { Heading, RootContent } from "mdast";
import type { Detector, DetectorHit } from "./types.js";
import { nodeText } from "./nodeText.js";

export interface FaqItem {
  question: string;
  answer: RootContent[];
}
export interface FaqData {
  items: FaqItem[];
}

const Q_PREFIX = /^Q[:.\)]\s*/i;

/**
 * FAQ: a run of `### Q:`-style headings, each followed by answer nodes until the next Q heading.
 * Needs at least two question/answer pairs to be worth an accordion.
 */
export const faqDetector: Detector = (nodes, index, hint): DetectorHit | null => {
  const start = nodes[index];
  if (start?.type !== "heading") return null;
  const depth = (start as Heading).depth;

  const isQ = (n: RootContent | undefined): boolean =>
    n?.type === "heading" && (n as Heading).depth === depth && Q_PREFIX.test(nodeText(n).trim());

  if (!isQ(start)) return null;

  const items: FaqItem[] = [];
  let i = index;
  while (i < nodes.length && isQ(nodes[i])) {
    const question = nodeText(nodes[i]).trim().replace(Q_PREFIX, "");
    const answer: RootContent[] = [];
    i += 1;
    while (i < nodes.length && !isQ(nodes[i])) {
      // Stop the FAQ run if we hit a heading shallower than the question level.
      if (nodes[i].type === "heading" && (nodes[i] as Heading).depth < depth) break;
      answer.push(nodes[i]);
      i += 1;
    }
    items.push({ question, answer });
  }

  if (items.length < 2) return null;
  const consumed = i - index;
  const data: FaqData = { items };

  if (hint === "faq") return { kind: "faq", confidence: 1, consumed, data, forced: true };
  return { kind: "faq", confidence: 0.9, consumed, data };
};
