import { tableNodes, type TableNodes } from "prosemirror-tables";

export const MARKDOWN_TABLE_MAX_ROWS = 200;
export const MARKDOWN_TABLE_MAX_COLUMNS = 50;
export const MARKDOWN_TABLE_MIN_ROWS = 2;
export const MARKDOWN_TABLE_MIN_COLUMNS = 1;

export type MarkdownTableAlignment = "left" | "right" | "center" | null;

export interface MarkdownTableSourceAttrs {
  readonly prefix?: string;
  readonly rawSource?: string;
  readonly rawText?: string;
  readonly sourceFrom?: number;
  readonly sourceTo?: number;
  readonly prefixFrom?: number;
  readonly prefixTo?: number;
  readonly align?: readonly MarkdownTableAlignment[] | null;
}

interface TableCellDomNode {
  readonly attrs: {
    readonly colspan?: number;
    readonly rowspan?: number;
    readonly colwidth?: readonly number[] | null;
    readonly align?: MarkdownTableAlignment | null;
  };
}

/**
 * Node specs for a GFM table that can be merged into the Markdown schema.
 * `prosemirror-tables` owns table roles, DOM shape, and cell attributes;
 * Markdown adds only source identity plus column alignment metadata.
 */
const baseTableNodes = tableNodes({
  tableGroup: "block",
  cellContent: "inline*",
  cellAttributes: {},
});

export const markdownTableNodeSpecs: TableNodes = {
  table: {
    ...baseTableNodes.table,
    attrs: {
      prefix: { default: "" },
      rawSource: { default: "" },
      rawText: { default: "" },
      sourceFrom: { default: -1 },
      sourceTo: { default: -1 },
      prefixFrom: { default: -1 },
      prefixTo: { default: -1 },
      align: { default: null },
    },
    // A wrapper keeps wide tables contained inside the live reading surface without
    // making the whole document horizontally scroll. Its child table remains the
    // semantic table element used by ProseMirror's table editing plugin.
    toDOM: () => [
      "div",
      { class: "vr-pm-table-scroll", "data-vr-markdown-table": "true" },
      ["table", { class: "vr-pm-table" }, ["tbody", 0]],
    ],
  },
  table_row: {
    ...baseTableNodes.table_row,
    toDOM: () => ["tr", { class: "vr-pm-table-row" }, 0],
  },
  table_cell: {
    ...baseTableNodes.table_cell,
    attrs: { ...baseTableNodes.table_cell.attrs, align: { default: null } },
    toDOM: (node) => tableCellDom("td", node as TableCellDomNode),
  },
  table_header: {
    ...baseTableNodes.table_header,
    attrs: { ...baseTableNodes.table_header.attrs, align: { default: null } },
    toDOM: (node) => tableCellDom("th", node as TableCellDomNode),
  },
};

function tableCellDom(tag: "td" | "th", node: TableCellDomNode) {
  const alignment = node.attrs.align ?? "left";
  const attrs: Record<string, string | number> = {
    class: tag === "th" ? "vr-pm-table-cell vr-pm-table-header" : "vr-pm-table-cell",
    "data-vr-table-align": alignment,
    style: `text-align: ${alignment};`,
  };
  if (node.attrs.colspan && node.attrs.colspan > 1) attrs.colspan = node.attrs.colspan;
  if (node.attrs.rowspan && node.attrs.rowspan > 1) attrs.rowspan = node.attrs.rowspan;
  if (node.attrs.colwidth?.length) attrs["data-colwidth"] = node.attrs.colwidth.join(",");
  return [tag, attrs, 0] as const;
}

export function normalizeTableAlignment(
  align: readonly (MarkdownTableAlignment | undefined)[] | null | undefined,
  columns: number,
): MarkdownTableAlignment[] {
  return Array.from({ length: columns }, (_, index) => align?.[index] ?? null);
}
