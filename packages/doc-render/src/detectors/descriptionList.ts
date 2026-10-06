import type { Detector, DetectorHit } from "./types";
import type { DescriptionList } from "../remarkDefListAbbr.js";
import { nodeText } from "./nodeText.js";

export interface DescriptionListData {
  items: { terms: string[]; details: string[] }[];
}

/** Pandoc-style definition list (semantic component). */
export const descriptionListDetector: Detector = (nodes, index, hint): DetectorHit | null => {
  const node = nodes[index];
  if (!node || (node.type as string) !== "descriptionList") return null;
  const list = node as unknown as DescriptionList;
  const items = list.children.map((item) => ({
    terms: item.terms.map((t) => nodeText(t as unknown as Parameters<typeof nodeText>[0]).trim()),
    details: item.details.map((d) =>
      nodeText(d as unknown as Parameters<typeof nodeText>[0]).trim(),
    ),
  }));

  const data: DescriptionListData = { items };
  if (hint === "descriptionList")
    return { kind: "descriptionList", confidence: 1, consumed: 1, data, forced: true };
  return { kind: "descriptionList", confidence: 0.9, consumed: 1, data };
};
