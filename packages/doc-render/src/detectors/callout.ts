import type { Blockquote, RootContent } from "mdast";
import type { Detector, DetectorHit } from "./types.js";
import { nodeText } from "./nodeText.js";

const CALLOUT_TYPES = ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"] as const;
type CalloutType = (typeof CALLOUT_TYPES)[number];

export interface CalloutData {
  type: CalloutType;
  title: string | null;
  /** Remaining blockquote children to render as the callout body. */
  body: RootContent[];
}

const MARKER = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*(.*)$/;

/** GFM callout: a blockquote whose first line is `[!TYPE]` (optionally followed by a title). */
export const calloutDetector: Detector = (nodes, index): DetectorHit | null => {
  const node = nodes[index];
  if (!node || node.type !== "blockquote") return null;
  const bq = node as Blockquote;
  const first = bq.children[0];
  if (!first || first.type !== "paragraph") return null;

  // remark merges the whole first paragraph (marker + any following lines) into one text node.
  // Match the marker on the first line only, then keep the rest as body.
  const [firstLine, ...restLines] = nodeText(first).trimStart().split(/\r?\n/);
  const m = MARKER.exec(firstLine);
  if (!m) return null;

  const type = m[1] as CalloutType;
  const inlineTitle = m[2]?.trim() || null;

  const body: RootContent[] = bq.children.slice(1);
  const remainder = restLines.join("\n").trim();
  if (remainder) {
    // Body text that shared the marker paragraph — re-emit as a plain paragraph.
    body.unshift({
      type: "paragraph",
      children: [{ type: "text", value: remainder }],
    } as unknown as RootContent);
  }

  const data: CalloutData = { type, title: inlineTitle, body };
  return { kind: "callout", confidence: 1, consumed: 1, data };
};
