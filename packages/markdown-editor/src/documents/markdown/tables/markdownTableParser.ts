import type { PhrasingContent, Table, TableCell, TableRow } from "mdast";
import type { Node as ProseMirrorNode, Schema } from "prosemirror-model";
import {
  MARKDOWN_TABLE_MAX_COLUMNS,
  MARKDOWN_TABLE_MAX_ROWS,
  type MarkdownTableAlignment,
  normalizeTableAlignment,
} from "./markdownTableSchema";

export interface MarkdownTableSourceRange {
  readonly prefix?: string;
  readonly rawSource?: string;
  readonly rawText?: string;
  readonly sourceFrom?: number;
  readonly sourceTo?: number;
  readonly prefixFrom?: number;
  readonly prefixTo?: number;
}

export interface MarkdownTableParserOptions {
  readonly schema: Schema;
  readonly source?: MarkdownTableSourceRange;
  readonly parseInline: (children: readonly PhrasingContent[]) => readonly ProseMirrorNode[] | null;
}

export type MarkdownTableParseResult =
  | { readonly kind: "editable"; readonly node: ProseMirrorNode }
  | { readonly kind: "protected"; readonly reason: string };

/** Parse an mdast GFM table into the supplied schema's table node types. */
export function parseGfmTable(
  table: Table,
  options: MarkdownTableParserOptions,
): MarkdownTableParseResult {
  const tableType = options.schema.nodes.table;
  const rowType = options.schema.nodes.table_row;
  const headerType = options.schema.nodes.table_header;
  const cellType = options.schema.nodes.table_cell;
  if (!tableType || !rowType || !headerType || !cellType) {
    return { kind: "protected", reason: "Markdown schema has no table node types" };
  }

  const rows = table.children as readonly TableRow[];
  const columns = Math.max(0, ...rows.map((row) => row.children.length), table.align?.length ?? 0);
  if (!rows.length || rows.length > MARKDOWN_TABLE_MAX_ROWS) {
    return { kind: "protected", reason: "Table exceeds supported row limit" };
  }
  if (!columns || columns > MARKDOWN_TABLE_MAX_COLUMNS) {
    return { kind: "protected", reason: "Table exceeds supported column limit" };
  }

  const align = normalizeTableAlignment(
    (table.align ?? []) as readonly (MarkdownTableAlignment | undefined)[],
    columns,
  );
  const pmRows: ProseMirrorNode[] = [];
  for (const [rowIndex, row] of rows.entries()) {
    const cells: ProseMirrorNode[] = [];
    for (let columnIndex = 0; columnIndex < columns; columnIndex++) {
      const mdastCell = row.children[columnIndex];
      const inline = mdastCell ? options.parseInline(mdastCell.children) : [];
      if (inline === null) {
        return {
          kind: "protected",
          reason: `Unsupported inline content in row ${rowIndex + 1}, column ${columnIndex + 1}`,
        };
      }
      const type = rowIndex === 0 ? headerType : cellType;
      cells.push(
        type.create({ align: align[columnIndex] ?? null }, inline.length ? inline : undefined),
      );
    }
    pmRows.push(rowType.create(null, cells));
  }

  const rawText = rows
    .map((row) => row.children.map((cell) => cellText(cell)).join("|"))
    .join("\n");
  const attrs = filterTableAttrs(tableType, {
    ...options.source,
    rawText,
    align,
  });
  return { kind: "editable", node: tableType.create(attrs, pmRows) };
}

function filterTableAttrs(type: ProseMirrorNode["type"], attrs: Record<string, unknown>) {
  const declared = type.spec.attrs;
  if (!declared) return {};
  return Object.fromEntries(Object.entries(attrs).filter(([name]) => name in declared));
}

function cellText(cell: TableCell): string {
  return cell.children
    .map((child) => {
      if (child.type === "text" || child.type === "inlineCode") {
        return "value" in child && typeof child.value === "string" ? child.value : "";
      }
      return "children" in child && Array.isArray(child.children)
        ? child.children
            .map((nested) => ("value" in nested ? String(nested.value ?? "") : ""))
            .join("")
        : "";
    })
    .join("");
}
