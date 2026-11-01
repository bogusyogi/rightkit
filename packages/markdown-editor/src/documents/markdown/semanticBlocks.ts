// the host app semantic-block pipeline: @rightkit/doc-render's detectors, limited to the host app
// `<!-- viewright: KIND -->` hints, plus the `hit.data` payload shapes the package emits but does
// not export, and the mdast text flattener the host app shares.
import { buildBlocks as buildDocRenderBlocks, type Block } from "@rightkit/doc-render";
import type { RootContent } from "mdast";

export type { Block } from "@rightkit/doc-render";

const HINT_PREFIXES = ["viewright"] as const;

export function buildBlocks(nodes: RootContent[]): Block[] {
  return buildDocRenderBlocks(nodes, { hintPrefixes: HINT_PREFIXES });
}

/** True when `node` is a GFM `[!TYPE]` callout blockquote. */
export function isCallout(node: RootContent): boolean {
  const [block] = buildBlocks([node]);
  return block?.kind === "semantic" && block.hit.kind === "callout";
}

export type CalloutType = "NOTE" | "TIP" | "IMPORTANT" | "WARNING" | "CAUTION";
export interface CalloutData {
  type: CalloutType;
  title: string | null;
  /** Remaining blockquote children to render as the callout body. */
  body: RootContent[];
}
export interface FrontmatterData {
  /** Parsed key/value pairs (flat, string values only — deep YAML stays untouched). */
  fields: Array<{ key: string; value: string }>;
}
export interface StepperData {
  title: string | null;
  steps: string[];
}
export interface ComparisonData {
  left: { title: string; items: string[] };
  right: { title: string; items: string[] };
  /** "pro-con" tints the cards; "neutral" renders them evenly. */
  tone: "pro-con" | "neutral";
}
export interface FaqItem {
  question: string;
  answer: RootContent[];
}
export interface FaqData {
  items: FaqItem[];
}
export interface RichTableData {
  headers: string[];
  rows: string[][];
  /** Column index → true when every body cell parses as a number (enables sort + bar viz). */
  numericColumns: boolean[];
  align: Array<"left" | "right" | "center" | null>;
}
export interface DescriptionListData {
  items: { terms: string[]; details: string[] }[];
}

/** Flatten the visible text of an mdast node (headings, paragraphs, list items, etc.). */
export function nodeText(node: unknown): string {
  if (!node) return "";
  const anyNode = node as { value?: unknown; children?: unknown[] };
  if (typeof anyNode.value === "string") return anyNode.value;
  if (Array.isArray(anyNode.children)) return anyNode.children.map((c) => nodeText(c)).join("");
  return "";
}
