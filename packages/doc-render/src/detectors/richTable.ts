import type { Table, TableRow } from "mdast";
import type { Detector, DetectorHit } from "./types.js";
import { nodeText } from "./nodeText.js";

export interface RichTableData {
  headers: string[];
  rows: string[][];
  /** Column index → true when every body cell parses as a number (enables sort + bar viz). */
  numericColumns: boolean[];
  align: Array<"left" | "right" | "center" | null>;
}

function cells(row: TableRow): string[] {
  return row.children.map((c) => nodeText(c).trim());
}

function isNumeric(v: string): boolean {
  if (v === "") return false;
  return !Number.isNaN(Number(v.replace(/[,$%\s]/g, "")));
}

/** GFM table → sortable rich table. Always claims a table; confidence reflects how "data-like" it is. */
export const richTableDetector: Detector = (nodes, index, hint): DetectorHit | null => {
  const node = nodes[index];
  if (node?.type !== "table") return null;
  const table = node as Table;
  const [headRow, ...bodyRows] = table.children as TableRow[];
  if (!headRow) return null;

  const headers = cells(headRow);
  const rows = bodyRows.map(cells);
  const cols = headers.length;

  const numericColumns: boolean[] = [];
  for (let c = 0; c < cols; c++) {
    const colVals = rows.map((r) => r[c] ?? "");
    numericColumns[c] = colVals.length > 0 && colVals.every(isNumeric);
  }
  const align = (table.align ?? []).map((a) => a ?? null);
  const data: RichTableData = { headers, rows, numericColumns, align };

  if (hint === "richTable")
    return { kind: "richTable", confidence: 1, consumed: 1, data, forced: true };

  // A table with numeric columns benefits from sort/bars; a tiny 1-row table barely does.
  const hasNumeric = numericColumns.some(Boolean);
  const confidence = hasNumeric && rows.length >= 2 ? 0.85 : rows.length >= 2 ? 0.65 : 0.3;
  return { kind: "richTable", confidence, consumed: 1, data };
};
