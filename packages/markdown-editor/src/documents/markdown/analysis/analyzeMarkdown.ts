/**
 * Pure analysis of a Markdown snapshot.
 *
 * Walks the source-map and the logical source to produce a
 * {@link MarkdownAnalysis}. Never mutates input; never throws;
 * never touches the filesystem.
 */

import { topLevelNodes } from "../core/markdownSourceMapIndexes";
import type {
  MarkdownAnalysis,
  MarkdownFootnoteEntry,
  MarkdownIssue,
  MarkdownReferenceEntry,
} from "./types";
import { makeIssueId } from "./types";
import { isLocalMarkdownReference, splitMarkdownUrl } from "./markdownUrls";
import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes";
import type { MarkdownSourceNode } from "../core/MarkdownSourceMap";
import { parseToTree } from "../parseMarkdown";

const SEVERITY_RANK: Record<MarkdownIssue["severity"], number> = {
  error: 0,
  warning: 1,
  info: 2,
};

const CODE_RANK: Record<MarkdownIssue["code"], number> = {
  "md.frontmatter.invalid": 0,
  "md.heading.multiple-h1": 1,
  "md.heading.skipped-level": 2,
  "md.heading.duplicate-id": 3,
  "md.table.malformed": 4,
  "md.fence.unclosed": 5,
  "md.math.invalid": 6,
  "md.mermaid.invalid": 7,
  "md.code.unknown-language": 8,
  "md.html.blocked": 9,
  "md.image.missing-target": 10,
  "md.image.missing-alt": 11,
  "md.image.embedded-data": 12,
  "md.link.missing-target": 13,
  "md.link.missing-anchor": 14,
  "md.footnote.missing-definition": 15,
  "md.footnote.unused-definition": 16,
  "md.trailing-whitespace": 17,
};

/**
 * Run the analysis pipeline over `snapshot`.
 */
export function analyzeMarkdown(
  snapshot: MarkdownCoreSnapshot,
  options: { safeHtml?: boolean } = {},
): MarkdownAnalysis {
  const safeHtml = options.safeHtml ?? true;
  const source = snapshot.logicalSource;
  const sourceMap = snapshot.sourceMap;
  const headings = sourceMap.nodes.filter((n) => n.kind === "heading");
  const references = extractReferences(snapshot);
  const footnoteReferences = extractFootnotes(sourceMap, "footnote-reference");
  const footnoteDefinitions = extractFootnotes(sourceMap, "footnote-definition");
  const syntaxIssues: MarkdownIssue[] = [
    ...detectHeadingIssues(snapshot, headings),
    ...detectTableIssues(snapshot),
    ...detectFenceIssues(source),
    ...detectFootnotesIssues(footnoteReferences, footnoteDefinitions),
    ...detectLinkIssues(references),
    ...detectImageIssues(snapshot, references),
    ...detectTrailingWhitespace(source, sourceMap.nodes),
    ...detectFrontmatterIssues(source),
  ];
  if (!safeHtml) {
    syntaxIssues.push(...detectHtmlBlockedIssues(sourceMap));
  }
  sortIssues(syntaxIssues);
  return {
    revision: snapshot.revision,
    headings,
    references,
    footnoteReferences,
    footnoteDefinitions,
    syntaxIssues,
  };
}

/**
 * Extract link and image references from the source-map.
 */
function extractReferences(snapshot: MarkdownCoreSnapshot): MarkdownReferenceEntry[] {
  const out: MarkdownReferenceEntry[] = [];
  const urls = markdownUrlsByRange(snapshot.logicalSource);
  for (const node of snapshot.sourceMap.nodes) {
    if (node.kind === "link" || node.kind === "image") {
      const url = urls.get(`${node.kind}:${node.range.from}:${node.range.to}`) ?? null;
      if (url === null) continue;
      const { pathPart, fragment } = splitMarkdownUrl(url);
      out.push({
        rawUrl: url,
        kind: node.kind === "link" ? "link" : "image",
        range: node.range,
        nodeId: node.id,
        localPath: isLocalMarkdownReference(url) ? pathPart : null,
        fragment,
      });
    }
  }
  return out;
}

function markdownUrlsByRange(source: string): Map<string, string> {
  const urls = new Map<string, string>();
  const visit = (value: unknown): void => {
    if (!value || typeof value !== "object") return;
    const node = value as {
      type?: string;
      url?: unknown;
      position?: { start?: { offset?: number }; end?: { offset?: number } };
      children?: unknown[];
    };
    if ((node.type === "link" || node.type === "image") && typeof node.url === "string") {
      const from = node.position?.start?.offset;
      const to = node.position?.end?.offset;
      if (typeof from === "number" && typeof to === "number") {
        urls.set(`${node.type}:${from}:${to}`, node.url);
      }
    }
    for (const child of node.children ?? []) visit(child);
  };
  visit(parseToTree(source));
  return urls;
}

function extractFootnotes(
  map: { nodes: ReadonlyArray<MarkdownSourceNode> },
  kind: "footnote-reference" | "footnote-definition",
): MarkdownFootnoteEntry[] {
  return map.nodes
    .filter((n) => n.kind === kind)
    .map((n) => ({
      identifier: extractIdentifier(n),
      range: n.range,
      nodeId: n.id,
    }));
}

function extractIdentifier(node: MarkdownSourceNode): string {
  // Identifier is encoded in the semantic key as a plain string.
  return node.semanticKey;
}

function detectHeadingIssues(
  snapshot: MarkdownCoreSnapshot,
  headings: readonly MarkdownSourceNode[],
): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  let h1Count = 0;
  const previousDepths: number[] = [];
  const seenSlugs = new Set<string>();
  for (const heading of headings) {
    const depth = heading.depth ?? 0;
    if (depth === 1) h1Count++;
    if (previousDepths.length > 0) {
      const last = previousDepths[previousDepths.length - 1] as number;
      if (depth > last + 1) {
        out.push({
          id: makeIssueId("md.heading.skipped-level", heading.id, heading.range),
          code: "md.heading.skipped-level",
          severity: "info",
          message: `Heading skipped from level ${last} to ${depth}.`,
          range: heading.range,
          nodeId: heading.id,
        });
      }
    }
    previousDepths.push(depth);
    const slug = nodeTextForRange(snapshot.logicalSource, heading.range);
    if (slug) {
      if (seenSlugs.has(slug)) {
        out.push({
          id: makeIssueId("md.heading.duplicate-id", heading.id, heading.range),
          code: "md.heading.duplicate-id",
          severity: "warning",
          message: `Duplicate heading slug "${slug}".`,
          range: heading.range,
          nodeId: heading.id,
        });
      }
      seenSlugs.add(slug);
    }
  }
  if (h1Count > 1) {
    out.push({
      id: makeIssueId("md.heading.multiple-h1", undefined, { from: 0, to: 0 }),
      code: "md.heading.multiple-h1",
      severity: "warning",
      message: `Document has ${h1Count} H1 headings.`,
      range: { from: 0, to: 0 },
    });
  }
  return out;
}

function detectTableIssues(snapshot: MarkdownCoreSnapshot): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  for (const node of topLevelNodes(snapshot.sourceMap)) {
    if (node.kind !== "table") continue;
    const raw = snapshot.logicalSource.slice(node.range.from, node.range.to);
    const rows = raw.split("\n").filter((line) => /^\s*\|/.test(line));
    if (rows.length < 2) {
      out.push({
        id: makeIssueId("md.table.malformed", node.id, node.range),
        code: "md.table.malformed",
        severity: "error",
        message: "Table needs at least header and separator rows.",
        range: node.range,
        nodeId: node.id,
      });
      continue;
    }
    const columnCounts = rows.map((row) => row.split("|").length - 2);
    const headerCount = columnCounts[0] ?? 0;
    for (let i = 1; i < columnCounts.length; i++) {
      if (columnCounts[i] !== headerCount) {
        out.push({
          id: makeIssueId("md.table.malformed", node.id, node.range),
          code: "md.table.malformed",
          severity: "error",
          message: `Table row ${i + 1} column count mismatch.`,
          range: node.range,
          nodeId: node.id,
        });
        break;
      }
    }
  }
  return out;
}

function detectFenceIssues(source: string): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  const fencePattern = /^(```|~~~)/gm;
  const opens: { start: number; marker: string }[] = [];
  let match = fencePattern.exec(source) as RegExpExecArray | null;
  while (match !== null) {
    const marker = match[1] as string;
    const last = opens[opens.length - 1];
    if (last && last.marker === marker) {
      opens.pop();
    } else if (opens.length === 0) {
      opens.push({ start: match.index, marker });
    }
    match = fencePattern.exec(source) as RegExpExecArray | null;
  }
  for (const open of opens) {
    out.push({
      id: makeIssueId("md.fence.unclosed", undefined, { from: open.start, to: open.start + 3 }),
      code: "md.fence.unclosed",
      severity: "warning",
      message: "Code fence is not closed.",
      range: { from: open.start, to: open.start + 3 },
    });
  }
  return out;
}

function detectFootnotesIssues(
  references: readonly MarkdownFootnoteEntry[],
  definitions: readonly MarkdownFootnoteEntry[],
): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  const definedIds = new Set(definitions.map((d) => d.identifier));
  const referencedIds = new Set(references.map((r) => r.identifier));
  for (const ref of references) {
    if (!definedIds.has(ref.identifier)) {
      out.push({
        id: makeIssueId("md.footnote.missing-definition", ref.nodeId ?? undefined, ref.range),
        code: "md.footnote.missing-definition",
        severity: "warning",
        message: `Footnote "${ref.identifier}" is referenced but not defined.`,
        range: ref.range,
        nodeId: ref.nodeId ?? undefined,
      });
    }
  }
  for (const def of definitions) {
    if (!referencedIds.has(def.identifier)) {
      out.push({
        id: makeIssueId("md.footnote.unused-definition", def.nodeId ?? undefined, def.range),
        code: "md.footnote.unused-definition",
        severity: "info",
        message: `Footnote "${def.identifier}" is defined but unused.`,
        range: def.range,
        nodeId: def.nodeId ?? undefined,
      });
    }
  }
  return out;
}

function detectLinkIssues(references: readonly MarkdownReferenceEntry[]): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  for (const ref of references) {
    if (ref.kind !== "link") continue;
    if (ref.rawUrl.length === 0) {
      out.push({
        id: makeIssueId("md.link.missing-target", ref.nodeId ?? undefined, ref.range),
        code: "md.link.missing-target",
        severity: "error",
        message: "Link target is empty.",
        range: ref.range,
        nodeId: ref.nodeId ?? undefined,
      });
    } else if (ref.fragment && ref.localPath === null && !ref.rawUrl.startsWith("#")) {
      // Link points to a fragment of a remote URL — usually a mistake.
      out.push({
        id: makeIssueId("md.link.missing-anchor", ref.nodeId ?? undefined, ref.range),
        code: "md.link.missing-anchor",
        severity: "info",
        message: "Link target has a fragment but no local anchor target.",
        range: ref.range,
        nodeId: ref.nodeId ?? undefined,
      });
    }
  }
  return out;
}

function detectImageIssues(
  snapshot: MarkdownCoreSnapshot,
  references: readonly MarkdownReferenceEntry[],
): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  for (const ref of references) {
    if (ref.kind !== "image") continue;
    if (ref.rawUrl.length === 0) {
      out.push({
        id: makeIssueId("md.image.missing-target", ref.nodeId ?? undefined, ref.range),
        code: "md.image.missing-target",
        severity: "error",
        message: "Image target is empty.",
        range: ref.range,
        nodeId: ref.nodeId ?? undefined,
      });
    }
    if (ref.rawUrl.startsWith("data:")) {
      out.push({
        id: makeIssueId("md.image.embedded-data", ref.nodeId ?? undefined, ref.range),
        code: "md.image.embedded-data",
        severity: "info",
        message: "Image is inlined as a data URL.",
        range: ref.range,
        nodeId: ref.nodeId ?? undefined,
      });
    }
  }
  void snapshot;
  return out;
}

function detectTrailingWhitespace(
  source: string,
  nodes: ReadonlyArray<MarkdownSourceNode>,
): MarkdownIssue[] {
  const out: MarkdownIssue[] = [];
  const pattern = /[ \t]+(?=\n)/g;
  let match = pattern.exec(source) as RegExpExecArray | null;
  while (match !== null) {
    const start = match.index;
    const end = start + match[0].length;
    out.push({
      id: makeIssueId("md.trailing-whitespace", undefined, { from: start, to: end }),
      code: "md.trailing-whitespace",
      severity: "info",
      message: "Line has trailing whitespace.",
      range: { from: start, to: end },
    });
    match = pattern.exec(source) as RegExpExecArray | null;
  }
  void nodes;
  return out;
}

function detectFrontmatterIssues(source: string): MarkdownIssue[] {
  if (!source.startsWith("---\n")) return [];
  const closing = source.indexOf("\n---\n", 4);
  if (closing === -1) {
    return [
      {
        id: makeIssueId("md.frontmatter.invalid", undefined, { from: 0, to: 3 }),
        code: "md.frontmatter.invalid",
        severity: "warning",
        message: "Frontmatter block is not closed.",
        range: { from: 0, to: 3 },
      },
    ];
  }
  return [];
}

function detectHtmlBlockedIssues(map: {
  nodes: ReadonlyArray<MarkdownSourceNode>;
}): MarkdownIssue[] {
  return map.nodes
    .filter((n) => n.kind === "html")
    .map((n) => ({
      id: makeIssueId("md.html.blocked", n.id, n.range),
      code: "md.html.blocked" as const,
      severity: "warning" as const,
      message: "Raw HTML is blocked in safe mode.",
      range: n.range,
      nodeId: n.id,
    }));
}

/**
 * Stable sort: by range.from, then by severity, then by code rank.
 */
function sortIssues(issues: MarkdownIssue[]): void {
  issues.sort((a, b) => {
    const r = a.range.from - b.range.from;
    if (r !== 0) return r;
    const s = SEVERITY_RANK[a.severity] - SEVERITY_RANK[b.severity];
    if (s !== 0) return s;
    return CODE_RANK[a.code] - CODE_RANK[b.code];
  });
}

/**
 * Compute the slug-friendly plain text for a heading node. Used to
 * detect duplicate slugs without depending on the mdast-to-slug
 * map (which is keyed by object identity).
 */
function nodeTextForRange(source: string, range: { from: number; to: number }): string {
  const raw = source.slice(range.from, range.to);
  // Strip the leading `#` markers and any trailing newline so we
  // compare the text content only.
  const stripped = raw.replace(/^#+\s*/, "").replace(/\s+$/, "");
  return stripped.toLowerCase().replace(/\s+/g, "-");
}
