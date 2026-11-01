import type { Heading, RootContent } from "mdast";
import { parseToTree } from "./parseMarkdown";
import { nodeText } from "./semanticBlocks";
import { buildHeadingSlugs } from "./headingSlugs";
import type { OutlineHeading } from "./Outline";
import type { MarkdownSourceMap } from "./core/MarkdownSourceMap";
import { stripMarkdown } from "./export/exportMarkdown";
import { hostAppName } from "../../appName";

export interface MarkdownHeading {
  depth: number;
  text: string;
  line: number;
}

export interface MappedMarkdownHeading extends MarkdownHeading {
  id: string;
}

export function collectMarkdownHeadings(source: string): MarkdownHeading[] {
  return parseToTree(source)
    .children.filter(
      (node): node is Heading => node.type === "heading" && !!node.position?.start.line,
    )
    .map((node) => ({
      depth: node.depth,
      text: nodeText(node),
      line: node.position?.start.line ?? 1,
    }));
}

export function collectSourceMapHeadings(
  source: string,
  sourceMap: MarkdownSourceMap,
): MappedMarkdownHeading[] {
  const headings: MappedMarkdownHeading[] = [];
  let cursor = 0;
  let line = 1;
  for (const node of sourceMap.nodes) {
    if (node.kind !== "heading" || !node.depth) continue;
    while (cursor < node.range.from) if (source.charCodeAt(cursor++) === 10) line++;
    headings.push({
      depth: node.depth,
      text: stripMarkdown(source.slice(node.range.from, node.range.to)),
      line,
      id: node.headingId ?? "",
    });
  }
  return headings;
}

export function sourceLineForNode(node: RootContent): number | null {
  return node.position?.start.line ?? null;
}

export function firstSourceLine(nodes: RootContent[]): number | null {
  for (const node of nodes) {
    const line = sourceLineForNode(node);
    if (line != null) return line;
  }
  return null;
}

export function insertTableTemplate(rows = 3, cols = 3): string {
  const headers = Array.from({ length: cols }, (_, i) => `Column ${i + 1}`);
  const divider = Array.from({ length: cols }, () => "---");
  const body = Array.from({ length: Math.max(1, rows - 1) }, () =>
    Array.from({ length: cols }, () => "Cell"),
  );
  return `${[...[headers, divider], ...body].map((row) => `| ${row.join(" | ")} |`).join("\n")}\n`;
}

export function formatMarkdownTables(source: string): string {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const out = [...lines];
  for (let i = 0; i < lines.length; i++) {
    if (!isTableDivider(lines[i])) continue;
    const start = i > 0 && isTableRow(lines[i - 1]) ? i - 1 : i;
    let end = i;
    while (end + 1 < lines.length && isTableRow(lines[end + 1])) end++;
    const block = lines.slice(start, end + 1);
    const formatted = formatTableBlock(block);
    if (formatted) out.splice(start, block.length, ...formatted);
    i = end;
  }
  return out.join("\n");
}

export const DIAGRAM_TEMPLATES = [
  {
    label: "Flowchart",
    code: "flowchart TD\n  Start([Start]) --> Step[Review document]\n  Step --> Done([Done])",
  },
  {
    label: "Sequence",
    code: `sequenceDiagram\n  participant User\n  participant ${hostAppName()}\n  User->>${hostAppName()}: Open Markdown\n  ${hostAppName()}-->>User: Render preview`,
  },
  {
    label: "Class",
    code: "classDiagram\n  class Document {\n    +path\n    +format\n  }\n  class Preview\n  Document --> Preview",
  },
  {
    label: "State",
    code: "stateDiagram-v2\n  [*] --> Preview\n  Preview --> Source\n  Source --> Preview",
  },
  {
    label: "ER",
    code: "erDiagram\n  DOCUMENT ||--o{ HEADING : contains\n  DOCUMENT {\n    string path\n    string format\n  }",
  },
  {
    label: "Journey",
    code: "journey\n  title Review a document\n  section Open\n    Pick file: 5: User\n  section Inspect\n    Compare source and preview: 5: User",
  },
  {
    label: "Gantt",
    code: "gantt\n  title Launch checklist\n  dateFormat  YYYY-MM-DD\n  section Build\n  Markdown QA :done, 2026-06-14, 1d\n  Release :active, 2026-06-15, 1d",
  },
  {
    label: "Pie",
    code: 'pie title Document mix\n  "Markdown" : 40\n  "PDF" : 30\n  "DOCX" : 20\n  "Images" : 10',
  },
  {
    label: "Git",
    code: "gitGraph\n  commit\n  branch polish\n  checkout polish\n  commit\n  checkout main\n  merge polish",
  },
  {
    label: "Mindmap",
    code: `mindmap\n  root((${hostAppName()}))\n    Source\n    Preview\n    Local files`,
  },
  {
    label: "Timeline",
    code: `timeline\n  title ${hostAppName()} Markdown\n  Source : edit truth\n  Preview : inspect result\n  QA : verify both`,
  },
  {
    label: "Quadrant",
    code: "quadrantChart\n  title Markdown priorities\n  x-axis Low effort --> High effort\n  y-axis Low value --> High value\n  Sync scroll: [0.35, 0.85]\n  Inline editor: [0.9, 0.35]",
  },
  {
    label: "Requirement",
    code: "requirementDiagram\n  requirement local_first {\n    id: VR-1\n    text: Files stay local\n    risk: low\n    verifymethod: test\n  }",
  },
  {
    label: "Sankey",
    code: "sankey-beta\n  Source,Preview,10\n  Preview,Decision,7\n  Preview,Edit,3",
  },
  {
    label: "XY Chart",
    code: 'xychart-beta\n  title "Load time"\n  x-axis [Open, Parse, Render]\n  y-axis "ms" 0 --> 100\n  bar [12, 24, 32]',
  },
  {
    label: "Block",
    code: 'block-beta\n  columns 3\n  source["Source"] preview["Preview"] qa["QA"]\n  source --> preview\n  preview --> qa',
  },
  {
    label: "Packet",
    code: 'packet-beta\n  title Markdown event\n  0-15: "source line"\n  16-31: "preview block"',
  },
] as const;

export function mermaidBlock(template: string = DIAGRAM_TEMPLATES[0].code): string {
  return `\`\`\`mermaid\n${template}\n\`\`\`\n`;
}

function isTableRow(line: string): boolean {
  const trimmed = line.trim();
  return trimmed.startsWith("|") && trimmed.endsWith("|") && trimmed.includes("|");
}

function isTableDivider(line: string): boolean {
  if (!isTableRow(line)) return false;
  return splitTableRow(line).every((cell) => /^:?-{3,}:?$/.test(cell.trim()));
}

function splitTableRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function formatTableBlock(lines: string[]): string[] | null {
  if (lines.length < 2) return null;
  const rows = lines.map(splitTableRow);
  const cols = Math.max(...rows.map((row) => row.length));
  const widths = Array.from({ length: cols }, (_, col) =>
    Math.max(3, ...rows.map((row) => (row[col] ?? "").length)),
  );
  return rows.map((row, rowIndex) => {
    const cells = Array.from({ length: cols }, (_, col) => {
      const cell = row[col] ?? "";
      if (rowIndex === 1 && /^:?-{3,}:?$/.test(cell)) return normalizeDivider(cell, widths[col]);
      return cell.padEnd(widths[col], " ");
    });
    return `| ${cells.join(" | ")} |`;
  });
}

function normalizeDivider(cell: string, width: number): string {
  const left = cell.startsWith(":");
  const right = cell.endsWith(":");
  const dashes = "-".repeat(Math.max(3, width - (left ? 1 : 0) - (right ? 1 : 0)));
  return `${left ? ":" : ""}${dashes}${right ? ":" : ""}`.padEnd(width, " ");
}

export function outlineHeadingsFor(source: string): OutlineHeading[] {
  try {
    const tree = parseToTree(source);
    const slugs = buildHeadingSlugs(tree);
    return tree.children
      .filter((node): node is Heading => node.type === "heading" && node.depth <= 3)
      .map((node) => ({ depth: node.depth, text: nodeText(node), id: slugs.get(node) ?? "" }));
  } catch {
    return [];
  }
}

export function shouldParseMarkdownPaste(text: string) {
  return (
    text.includes("\n") ||
    /(^|\n)\s{0,3}(#{1,6}\s|>\s|[-*+]\s|\d+\.\s|```|~~~)/.test(text) ||
    /[*_`[\]]/.test(text)
  );
}
