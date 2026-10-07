import type { Detector, DetectorHit } from "./types.js";

export interface FrontmatterData {
  /** Parsed key/value pairs (flat, string values only — deep YAML stays untouched). */
  fields: Array<{ key: string; value: string }>;
  raw: string;
}

/** `remark-frontmatter` emits a `yaml` node at the document head. We flatten top-level scalars. */
export const frontmatterDetector: Detector = (nodes, index): DetectorHit | null => {
  const node = nodes[index];
  // The yaml node type is added by remark-frontmatter; guard structurally.
  if (!node || (node.type as string) !== "yaml") return null;
  const raw = (node as unknown as { value: string }).value ?? "";

  const fields: FrontmatterData["fields"] = [];
  for (const line of raw.split(/\r?\n/)) {
    const m = /^([A-Za-z0-9_-]+)\s*:\s*(.*)$/.exec(line);
    if (m && m[2] !== "") fields.push({ key: m[1], value: m[2].replace(/^["']|["']$/g, "") });
  }

  // Only worth a header card if there's at least one usable field.
  if (fields.length === 0) return null;

  return {
    kind: "frontmatter",
    confidence: 1,
    consumed: 1,
    data: { fields, raw } satisfies FrontmatterData,
  };
};
