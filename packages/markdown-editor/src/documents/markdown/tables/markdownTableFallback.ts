import type { Table } from "mdast";
import { MARKDOWN_TABLE_MAX_COLUMNS, MARKDOWN_TABLE_MAX_ROWS } from "./markdownTableSchema";

export interface ProtectedMarkdownTableFallback {
  readonly kind: "protected-table";
  readonly editor: "source-grid";
  readonly source: string;
  readonly from: number;
  readonly to: number;
  readonly reason: string;
}

/** Adapter contract for rich tables that must remain in the source-grid editor. */
export interface MarkdownTableFallbackAdapter {
  classify(
    table: Table,
    source: string,
    from: number,
    to: number,
  ): ProtectedMarkdownTableFallback | null;
}

export function createMarkdownTableFallbackAdapter(): MarkdownTableFallbackAdapter {
  return {
    classify(table, source, from, to) {
      const rows = table.children;
      const columns = Math.max(0, ...rows.map((row) => row.children.length));
      if (!rows.length || rows.length > MARKDOWN_TABLE_MAX_ROWS) {
        return fallback(source, from, to, "Table exceeds supported row limit");
      }
      if (!columns || columns > MARKDOWN_TABLE_MAX_COLUMNS) {
        return fallback(source, from, to, "Table exceeds supported column limit");
      }
      for (const row of rows) {
        for (const cell of row.children) {
          if (
            cell.children.some(
              (child) => child.type === "html" || child.type === "mdxJsxTextElement",
            )
          ) {
            return fallback(source, from, to, "Table contains unsupported HTML content");
          }
        }
      }
      return null;
    },
  };
}

export function protectedMarkdownTableFallback(
  source: string,
  from: number,
  to: number,
  reason: string,
): ProtectedMarkdownTableFallback {
  return fallback(source, from, to, reason);
}

function fallback(
  source: string,
  from: number,
  to: number,
  reason: string,
): ProtectedMarkdownTableFallback {
  return {
    kind: "protected-table",
    editor: "source-grid",
    source: source.slice(from, to),
    from,
    to,
    reason,
  };
}
