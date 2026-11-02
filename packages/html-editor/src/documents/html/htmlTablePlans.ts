/**
 * Table planners for the HTML operation planner set (split out of
 * `htmlOperationPlanner.ts`, code moved verbatim): the span-aware table grid
 * model and the row/column insert/delete surgeries built on the shared
 * location kernel in `htmlPlanLocations.ts`.
 */
import {
  HtmlOperationError,
  locateElement,
  ownedDescendants,
  patch,
  type HtmlElementLocation,
} from "./htmlPlanLocations.js";
import type {
  HtmlMutationPlan,
  HtmlSourcePatch,
  HtmlSourceRange,
  HtmlRevision,
} from "./htmlMutationEngine.js";

// ── Tables ───────────────────────────────────────────────────────────────────

function cellsOfRow(source: string, row: HtmlElementLocation): HtmlElementLocation[] {
  return ownedDescendants(source, row.range, ["td", "th"], "tr");
}

function cellCount(row: HtmlElementLocation, source: string): number {
  return cellsOfRow(source, row).length;
}

/**
 * A table grid slot: the authored cell owning one grid column in one row.
 * `covered` is true when the slot is a rowspan continuation from an earlier row
 * and therefore has NO authored cell of its own in this row.
 */
interface GridSlot {
  cell: HtmlElementLocation;
  gridStart: number;
  gridEnd: number;
  covered: boolean;
}

interface TableGridRow {
  row: HtmlElementLocation;
  cells: Array<{ cell: HtmlElementLocation; gridStart: number; gridEnd: number }>;
}

interface TableGrid {
  rows: TableGridRow[];
  /** slots[rowIndex] maps grid column → owning slot (authored or rowspan-covered). */
  slots: Array<Map<number, GridSlot>>;
  columnCount: number;
}

function readSpanAttr(
  source: string,
  cell: HtmlElementLocation,
  name: "colspan" | "rowspan",
): number {
  const valueRange = cell.attrValues[name];
  if (!valueRange) return 1;
  const n = Number.parseInt(source.slice(valueRange.start, valueRange.end), 10);
  // Clamp to the HTML limit: an authored `colspan="999999999"` would otherwise make
  // the grid allocate ~1e9 slots and hang the renderer on any column operation.
  return Number.isFinite(n) && n >= 1 ? Math.min(n, 1000) : 1;
}

/**
 * Build the HTML table grid: expand each authored cell by its colspan and carry
 * rowspan occupancy down to following rows, so a cell maps to a grid-column
 * RANGE rather than a positional ordinal. Column surgery then resolves the
 * anchor to its grid column and finds the cell occupying that SAME column in
 * every row, instead of assuming cell[i] means column i.
 */
function buildTableGrid(
  source: string,
  tableRange: HtmlSourceRange,
  table: HtmlElementLocation,
): TableGrid {
  const rows = rowsOfTable(source, tableRange, table);
  const slots: Array<Map<number, GridSlot>> = [];
  const gridRows: TableGridRow[] = [];
  let columnCount = 0;
  for (let r = 0; r < rows.length; r++) {
    const row = rows[r];
    const cells = cellsOfRow(source, row);
    let rowMap = slots[r];
    if (!rowMap) {
      rowMap = new Map<number, GridSlot>();
      slots[r] = rowMap;
    }
    const rowCells: TableGridRow["cells"] = [];
    let col = 0;
    for (const cell of cells) {
      while (rowMap.has(col)) col++; // skip columns covered by a rowspan from above
      const colspan = readSpanAttr(source, cell, "colspan");
      const rowspan = readSpanAttr(source, cell, "rowspan");
      const gridStart = col;
      const gridEnd = col + colspan;
      for (let c = col; c < gridEnd; c++)
        rowMap.set(c, { cell, gridStart, gridEnd, covered: false });
      for (let k = 1; k < rowspan; k++) {
        const targetRow = r + k;
        if (targetRow >= rows.length) break;
        let targetMap = slots[targetRow];
        if (!targetMap) {
          targetMap = new Map<number, GridSlot>();
          slots[targetRow] = targetMap;
        }
        for (let c = col; c < gridEnd; c++)
          targetMap.set(c, { cell, gridStart, gridEnd, covered: true });
      }
      rowCells.push({ cell, gridStart, gridEnd });
      col = gridEnd;
    }
    gridRows.push({ row, cells: rowCells });
    columnCount = Math.max(columnCount, col);
  }
  return { rows: gridRows, slots, columnCount };
}

function anchorGridPosition(
  grid: TableGrid,
  anchorCell: HtmlElementLocation,
): { gridStart: number; gridEnd: number } | null {
  for (const gridRow of grid.rows) {
    const found = gridRow.cells.find((c) => c.cell.range.start === anchorCell.range.start);
    if (found) return { gridStart: found.gridStart, gridEnd: found.gridEnd };
  }
  return null;
}

/**
 * Authored rows whose NEAREST table ancestor is the target table. Column surgery must never reach
 * into a nested table: `descendantElements` walks everything inside the outer range, so without
 * this filter a column insert/delete would also add/remove cells in every nested table.
 */
function rowsOfTable(
  source: string,
  tableRange: HtmlSourceRange,
  _table: HtmlElementLocation,
): HtmlElementLocation[] {
  return ownedDescendants(source, tableRange, ["tr"], "table");
}

/** Insert one row (matching the anchor row's column count) before/after the anchor row. */
export function tableInsertRowPlan(
  source: string,
  tableRange: HtmlSourceRange,
  anchorRowRange: HtmlSourceRange,
  position: "before" | "after",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const table = locateElement(source, tableRange);
  if (!table || table.tagName !== "table")
    throw new HtmlOperationError("target-unresolved", "Table target does not resolve in source.");
  const anchor = locateElement(source, anchorRowRange);
  if (!anchor || anchor.tagName !== "tr")
    throw new HtmlOperationError("target-unresolved", "Anchor row does not resolve in source.");
  const grid = buildTableGrid(source, tableRange, table);
  // Use the grid's full column count (spans expanded), not the anchor row's authored
  // cell count, so the inserted row is never ragged against a spanned table.
  const columns = Math.max(1, grid.columnCount || cellCount(anchor, source));
  const cells = Array.from({ length: columns }, () => "<td></td>").join("");
  const insertion = position === "after" ? anchor.range.end : anchor.range.start;
  return {
    operation: "tableInsertRow",
    expectedRevision,
    mutationBoundary: { ...tableRange },
    impactBoundary: { ...tableRange },
    patches: [patch({ start: insertion, end: insertion }, "", `<tr>${cells}</tr>`)],
  };
}

/**
 * Insert one column: for EVERY authored row, insert a cell at the anchor column
 * index. One insertion per row — the multi-patch plan preserves all other bytes.
 */
export function tableInsertColumnPlan(
  source: string,
  tableRange: HtmlSourceRange,
  anchorCellRange: HtmlSourceRange,
  position: "before" | "after",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const table = locateElement(source, tableRange);
  if (!table || table.tagName !== "table")
    throw new HtmlOperationError("target-unresolved", "Table target does not resolve in source.");
  const anchorCell = locateElement(source, anchorCellRange);
  if (!anchorCell || (anchorCell.tagName !== "td" && anchorCell.tagName !== "th"))
    throw new HtmlOperationError("target-unresolved", "Anchor cell does not resolve in source.");

  const grid = buildTableGrid(source, tableRange, table);
  if (grid.rows.length === 0)
    throw new HtmlOperationError("target-unresolved", "Table has no authored rows.");
  const anchor = anchorGridPosition(grid, anchorCell);
  if (!anchor)
    throw new HtmlOperationError("target-unresolved", "Anchor cell is not in a row of the table.");

  // Insert after the anchor's full grid span (or before its start), never clamp an
  // out-of-range column to a neighbouring cell.
  const target = position === "after" ? anchor.gridEnd : anchor.gridStart;

  const cellTag = anchorCell.tagName;
  const patches: HtmlSourcePatch[] = [];
  for (const gridRow of grid.rows) {
    const pivot =
      position === "after"
        ? gridRow.cells.find((c) => c.gridEnd === target)
        : gridRow.cells.find((c) => c.gridStart === target);
    if (!pivot)
      throw new HtmlOperationError(
        "ambiguous",
        "Column insert would split a spanned or short row; refusing.",
      );
    const insertion = position === "after" ? pivot.cell.range.end : pivot.cell.range.start;
    patches.push(patch({ start: insertion, end: insertion }, "", `<${cellTag}></${cellTag}>`));
  }
  if (patches.length === 0)
    throw new HtmlOperationError("target-unresolved", "No row could host the new column.");
  return {
    operation: "tableInsertColumn",
    expectedRevision,
    mutationBoundary: { ...tableRange },
    impactBoundary: { ...tableRange },
    patches,
  };
}

/** Remove one row (single-patch element removal). */
export function tableDeleteRowPlan(
  source: string,
  tableRange: HtmlSourceRange,
  anchorRowRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const table = locateElement(source, tableRange);
  if (!table || table.tagName !== "table")
    throw new HtmlOperationError("target-unresolved", "Table target does not resolve in source.");
  const anchor = locateElement(source, anchorRowRange);
  if (!anchor || anchor.tagName !== "tr")
    throw new HtmlOperationError("target-unresolved", "Anchor row does not resolve in source.");

  const grid = buildTableGrid(source, tableRange, table);
  const rowIndex = grid.rows.findIndex((gridRow) => gridRow.row.range.start === anchor.range.start);
  if (rowIndex === -1)
    throw new HtmlOperationError("target-unresolved", "Anchor row is not in the target table.");

  // Refuse when deleting this row would orphan a rowspan: either a cell authored in
  // this row spans DOWN into rows below, or a cell from an earlier row spans INTO
  // this row (this row's slot map holds a `covered` continuation).
  const rowspansIntoBelow = grid.rows[rowIndex].cells.some(
    (c) => readSpanAttr(source, c.cell, "rowspan") > 1,
  );
  const coveredByAbove = Array.from(grid.slots[rowIndex]?.values() ?? []).some(
    (slot) => slot.covered,
  );
  if (rowspansIntoBelow || coveredByAbove)
    throw new HtmlOperationError("ambiguous", "Row delete would orphan a rowspan; refusing.");

  return {
    operation: "tableDeleteRow",
    expectedRevision,
    mutationBoundary: { ...tableRange },
    impactBoundary: { ...tableRange },
    patches: [patch(anchor.range, source.slice(anchor.range.start, anchor.range.end), "")],
  };
}

/** Remove one column: delete every row's cell at the anchor column index. */
export function tableDeleteColumnPlan(
  source: string,
  tableRange: HtmlSourceRange,
  anchorCellRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const table = locateElement(source, tableRange);
  if (!table || table.tagName !== "table")
    throw new HtmlOperationError("target-unresolved", "Table target does not resolve in source.");
  const anchorCell = locateElement(source, anchorCellRange);
  if (!anchorCell || (anchorCell.tagName !== "td" && anchorCell.tagName !== "th"))
    throw new HtmlOperationError("target-unresolved", "Anchor cell does not resolve in source.");

  const grid = buildTableGrid(source, tableRange, table);
  const anchor = anchorGridPosition(grid, anchorCell);
  if (!anchor)
    throw new HtmlOperationError("target-unresolved", "Anchor cell is not in a row of the table.");
  if (anchor.gridEnd - anchor.gridStart !== 1)
    throw new HtmlOperationError(
      "ambiguous",
      "Cannot delete a column from a colspan cell; refusing.",
    );

  const target = anchor.gridStart;
  const patches: HtmlSourcePatch[] = [];
  for (let r = 0; r < grid.rows.length; r++) {
    const slot = grid.slots[r].get(target);
    if (!slot || slot.covered)
      throw new HtmlOperationError(
        "ambiguous",
        "Column delete would leave a short or rowspan-covered row; refusing.",
      );
    if (slot.gridEnd - slot.gridStart !== 1)
      throw new HtmlOperationError(
        "ambiguous",
        "Column delete would split a colspan cell; refusing.",
      );
    patches.push(
      patch(slot.cell.range, source.slice(slot.cell.range.start, slot.cell.range.end), ""),
    );
  }
  return {
    operation: "tableDeleteColumn",
    expectedRevision,
    mutationBoundary: { ...tableRange },
    impactBoundary: { ...tableRange },
    patches,
  };
}
