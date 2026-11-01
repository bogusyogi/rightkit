/**
 * Table editor — pure model.
 *
 * Reads a Markdown table source range and provides row/column
 * operations that round-trip to a stable textual form. Pure:
 * never touches React or the document core directly.
 */

export interface TableCell {
  readonly text: string;
}

export interface TableRow {
  readonly cells: readonly TableCell[];
}

export interface MarkdownTableModel {
  readonly header: TableRow;
  readonly rows: readonly TableRow[];
  readonly alignments: readonly ("left" | "right" | "center" | null)[];
}

/**
 * Parse a Markdown table from `source` between `[from, to)`.
 * Returns `null` when the source is not a well-formed table.
 */
export function parseMarkdownTable(
  source: string,
  from: number,
  to: number,
): MarkdownTableModel | null {
  const slice = source.slice(from, to);
  const lines = slice.split("\n").filter((l) => /^\s*\|/.test(l));
  if (lines.length < 2) return null;
  const parseRow = (line: string): string[] => {
    const trimmed = line.replace(/^\s*\|/, "").replace(/\|\s*$/, "");
    return trimmed.split("|").map((c) => c.trim());
  };
  const headerCells = parseRow(lines[0] as string);
  const separator = parseRow(lines[1] as string);
  const alignments = separator.map((cell) => {
    const t = cell.trim();
    if (/^:-+:$/.test(t)) return "center" as const;
    if (/^:-+$/.test(t)) return "left" as const;
    if (/^-+:$/.test(t)) return "right" as const;
    return null;
  });
  const rows: TableRow[] = [];
  for (let i = 2; i < lines.length; i++) {
    const cells = parseRow(lines[i] as string);
    rows.push({
      cells: cells.map((text) => ({ text })),
    });
  }
  return {
    header: { cells: headerCells.map((text) => ({ text })) },
    rows,
    alignments,
  };
}

/**
 * Serialize the table back to Markdown.
 */
export function stringifyMarkdownTable(model: MarkdownTableModel): string {
  const headerRow = model.header.cells.map((c) => c.text).join(" | ");
  const sepRow = model.alignments
    .map((a) => {
      if (a === "left") return ":---";
      if (a === "right") return "---:";
      if (a === "center") return ":---:";
      return "---";
    })
    .join(" | ");
  const body = model.rows.map((r) => `| ${r.cells.map((c) => c.text).join(" | ")} |`).join("\n");
  return `| ${headerRow} |\n| ${sepRow} |\n${body ? `${body}\n` : ""}`;
}

/**
 * Insert a row at `rowIdx`. Returns a new model. Clamps the
 * index to `[0, rows.length]`.
 */
export function insertRow(
  model: MarkdownTableModel,
  rowIdx: number,
  row: TableRow,
): MarkdownTableModel {
  const idx = Math.max(0, Math.min(rowIdx, model.rows.length));
  const rows = [...model.rows.slice(0, idx), row, ...model.rows.slice(idx)];
  return { ...model, rows };
}

/**
 * Delete the row at `rowIdx`. Returns the same model if the
 * index is out of range.
 */
export function deleteRow(model: MarkdownTableModel, rowIdx: number): MarkdownTableModel {
  if (rowIdx < 0 || rowIdx >= model.rows.length) return model;
  return {
    ...model,
    rows: [...model.rows.slice(0, rowIdx), ...model.rows.slice(rowIdx + 1)],
  };
}

/**
 * Insert a column at `colIdx`. Pads existing rows that have fewer
 * cells than the header.
 */
export function insertColumn(
  model: MarkdownTableModel,
  colIdx: number,
  alignment: "left" | "right" | "center" | null,
): MarkdownTableModel {
  const idx = Math.max(0, Math.min(colIdx, model.header.cells.length));
  const header = {
    cells: [...model.header.cells.slice(0, idx), { text: "" }, ...model.header.cells.slice(idx)],
  };
  const rows = model.rows.map((r) => {
    const cells = [...r.cells.slice(0, idx), { text: "" }, ...r.cells.slice(idx)];
    return { cells };
  });
  const alignments = [...model.alignments.slice(0, idx), alignment, ...model.alignments.slice(idx)];
  return { header, rows, alignments };
}

/**
 * Delete the column at `colIdx`. Returns the same model if the
 * index is out of range.
 */
export function deleteColumn(model: MarkdownTableModel, colIdx: number): MarkdownTableModel {
  if (colIdx < 0 || colIdx >= model.header.cells.length) return model;
  const header = {
    cells: [...model.header.cells.slice(0, colIdx), ...model.header.cells.slice(colIdx + 1)],
  };
  const rows = model.rows.map((r) => ({
    cells: [...r.cells.slice(0, colIdx), ...r.cells.slice(colIdx + 1)],
  }));
  const alignments = [...model.alignments.slice(0, colIdx), ...model.alignments.slice(colIdx + 1)];
  return { header, rows, alignments };
}
