import type { Mark, Node as ProseMirrorNode } from "prosemirror-model";
import {
  MARKDOWN_TABLE_MAX_COLUMNS,
  MARKDOWN_TABLE_MAX_ROWS,
  type MarkdownTableAlignment,
  normalizeTableAlignment,
} from "./markdownTableSchema";

export interface GfmTableSerializerOptions {
  readonly serializeUnknownInline?: (node: ProseMirrorNode) => string;
}

/** Serialize one PM table node without reading `textContent`, preserving marks. */
export function serializeGfmTable(
  table: ProseMirrorNode,
  options: GfmTableSerializerOptions = {},
): string {
  const rows: ProseMirrorNode[] = [];
  table.forEach((row) => rows.push(row));
  if (!rows.length || rows.length > MARKDOWN_TABLE_MAX_ROWS) return "";
  const firstRow = rows[0];
  if (!firstRow) return "";

  const columnCount = rows.reduce((count, row) => Math.max(count, row.childCount), 0);
  if (!columnCount || columnCount > MARKDOWN_TABLE_MAX_COLUMNS) return "";

  const alignments = normalizeTableAlignment(
    table.attrs.align as readonly MarkdownTableAlignment[] | null | undefined,
    columnCount,
  );
  const lines = [serializeGfmTableRow(firstRow, columnCount, options)];
  lines.push(`| ${alignments.map((alignment) => alignmentMarker(alignment)).join(" | ")} |`);
  for (const row of rows.slice(1)) lines.push(serializeGfmTableRow(row, columnCount, options));
  return lines.join("\n");
}

export function serializeGfmTableRow(
  row: ProseMirrorNode,
  columnCount = row.childCount,
  options: GfmTableSerializerOptions = {},
): string {
  const cells = Array.from({ length: columnCount }, (_, index) => {
    const cell = row.child(index);
    return cell ? serializeGfmTableCell(cell, options) : "";
  });
  return `| ${cells.join(" | ")} |`;
}

export function serializeGfmTableCell(
  cell: ProseMirrorNode,
  options: GfmTableSerializerOptions = {},
): string {
  let markdown = "";
  cell.forEach((child) => {
    if (child.isText) {
      markdown += serializeTextNode(child);
      return;
    }
    if (child.type.name === "image") {
      const alt = escapeGfmPipes(String(child.attrs.alt ?? ""));
      const src = escapeDestination(String(child.attrs.src ?? ""));
      const title = child.attrs.title ? ` "${escapeTitle(String(child.attrs.title))}"` : "";
      markdown += `![${alt}](${src}${title})`;
      return;
    }
    if (child.type.name === "wiki_link") {
      const target = String(child.attrs.target ?? "");
      const hash = String(child.attrs.hash ?? "");
      const display = String(child.attrs.display ?? "");
      const href = target ? (hash ? `${target}#${hash}` : target) : `#${hash}`;
      markdown += `[[${escapeGfmPipes(href)}${display ? `|${escapeGfmPipes(display)}` : ""}]]`;
      return;
    }
    if (child.type.name === "citation") {
      const key = String(child.attrs.key ?? "");
      const locator = String(child.attrs.locator ?? "");
      markdown += `[@${escapeGfmPipes(key)}${locator ? ` ${escapeGfmPipes(locator)}` : ""}]`;
      return;
    }
    markdown += options.serializeUnknownInline?.(child) ?? "";
  });
  return markdown;
}

function serializeTextNode(node: ProseMirrorNode): string {
  const value = node.text ?? "";
  const parts = value.split("\n");
  let markdown = "";
  for (const [index, part] of parts.entries()) {
    if (index > 0) markdown += "<br>";
    // GFM splits table rows before parsing code spans, so code-cell pipes need escaping too.
    const escaped = escapeGfmPipes(part);
    markdown += serializeMarkedText(escaped, node.marks);
  }
  return markdown;
}

function serializeMarkedText(text: string, marks: readonly Mark[]): string {
  const codeMark = marks.some((mark) => mark.type.name === "code");
  let value = codeMark ? inlineCode(text) : text;
  if (marks.some((mark) => mark.type.name === "strong")) value = `**${value}**`;
  if (marks.some((mark) => mark.type.name === "em")) value = `*${value}*`;
  if (marks.some((mark) => mark.type.name === "delete")) value = `~~${value}~~`;
  if (marks.some((mark) => mark.type.name === "mark")) value = `==${value}==`;
  const link = marks.find((mark) => mark.type.name === "link");
  if (link) {
    const title = link.attrs.title ? ` "${escapeTitle(String(link.attrs.title))}"` : "";
    value = `[${value}](${escapeDestination(String(link.attrs.href ?? ""))}${title})`;
  }
  return value;
}

function inlineCode(value: string): string {
  const fence = value.includes("`") ? "``" : "`";
  return `${fence}${value}${fence}`;
}

/** Escape only unescaped pipes; this makes repeated serialization idempotent. */
export function escapeGfmPipes(value: string): string {
  let result = "";
  for (let index = 0; index < value.length; index++) {
    const char = value[index] ?? "";
    if (char !== "|") {
      result += char;
      continue;
    }
    let slashes = 0;
    for (let cursor = index - 1; cursor >= 0 && value[cursor] === "\\"; cursor--) slashes++;
    result += slashes % 2 === 1 ? "|" : "\\|";
  }
  return result;
}

function escapeDestination(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/ /g, "%20").replace(/\)/g, "\\)");
}

function escapeTitle(value: string): string {
  return value
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/[\r\n]/g, " ");
}

function alignmentMarker(alignment: MarkdownTableAlignment): string {
  if (alignment === "left") return ":---";
  if (alignment === "right") return "---:";
  if (alignment === "center") return ":---:";
  return "---";
}
