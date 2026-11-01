import { keymap } from "prosemirror-keymap";
import { Plugin as ProseMirrorPlugin } from "prosemirror-state";
import type { Command, EditorState, Plugin, Transaction } from "prosemirror-state";
import { NodeSelection, TextSelection } from "prosemirror-state";
import type { EditorView } from "prosemirror-view";
import type { Node as ProseMirrorNode, Schema } from "prosemirror-model";
import {
  addColumnAfter as pmAddColumnAfter,
  addColumnBefore as pmAddColumnBefore,
  addRow,
  addRowAfter as pmAddRowAfter,
  addRowBefore as pmAddRowBefore,
  deleteColumn as pmDeleteColumn,
  deleteRow as pmDeleteRow,
  deleteTable as pmDeleteTable,
  goToNextCell,
  isInTable,
  selectedRect,
  tableEditing,
  TableMap,
} from "prosemirror-tables";
import {
  MARKDOWN_TABLE_MAX_COLUMNS,
  MARKDOWN_TABLE_MAX_ROWS,
  MARKDOWN_TABLE_MIN_COLUMNS,
  MARKDOWN_TABLE_MIN_ROWS,
  type MarkdownTableAlignment,
  normalizeTableAlignment,
} from "./markdownTableSchema";
import { setMarkdownTransactionIntent } from "../projection/markdownTransactionIntent";

function dispatchTableTransaction(dispatch: (transaction: Transaction) => void) {
  return (transaction: Transaction) =>
    dispatch(setMarkdownTransactionIntent(transaction, "table-structural"));
}

export interface MarkdownTableInsertionTarget {
  readonly from: number;
  readonly to: number;
  readonly replacesEmptyBlock: boolean;
}

export function createMarkdownTable(
  schema: Schema,
  rows: number,
  columns: number,
  align: readonly MarkdownTableAlignment[] = [],
): ProseMirrorNode | null {
  if (
    rows < MARKDOWN_TABLE_MIN_ROWS ||
    rows > MARKDOWN_TABLE_MAX_ROWS ||
    columns < MARKDOWN_TABLE_MIN_COLUMNS ||
    columns > MARKDOWN_TABLE_MAX_COLUMNS
  ) {
    return null;
  }
  const tableType = schema.nodes.table;
  const rowType = schema.nodes.table_row;
  const headerType = schema.nodes.table_header;
  const cellType = schema.nodes.table_cell;
  if (!tableType || !rowType || !headerType || !cellType) return null;
  const normalizedAlign = normalizeTableAlignment(align, columns);
  const tableRows = Array.from({ length: rows }, (_, rowIndex) => {
    const cellTypeForRow = rowIndex === 0 ? headerType : cellType;
    return rowType.create(
      null,
      Array.from(
        { length: columns },
        (_, columnIndex) =>
          cellTypeForRow.createAndFill({ align: normalizedAlign[columnIndex] ?? null }) ??
          cellTypeForRow.create({ align: normalizedAlign[columnIndex] ?? null }),
      ),
    );
  });
  const attrs = tableType.spec.attrs?.align ? { align: normalizedAlign } : undefined;
  return tableType.create(attrs, tableRows);
}

/** Find the nearest legal block boundary, replacing only an empty text block. */
export function findMarkdownTableInsertionTarget(
  state: EditorState,
): MarkdownTableInsertionTarget | null {
  const tableRole = state.selection.$from.node(-1)?.type.spec.tableRole;
  if (tableRole === "table" || isInTable(state)) return null;
  const tableType = state.schema.nodes.table;
  if (!tableType) return null;
  const $from = state.selection.$from;
  if ($from.depth === 0 && state.selection instanceof NodeSelection) {
    const parent = $from.node(0);
    const child = state.selection.node;
    const index = $from.index(0);
    if (parent.canReplaceWith(index + 1, index + 1, tableType)) {
      const after = $from.pos + child.nodeSize;
      return { from: after, to: after, replacesEmptyBlock: false };
    }
    return null;
  }
  for (let depth = $from.depth; depth >= 1; depth--) {
    const parent = $from.node(depth - 1);
    const index = $from.index(depth - 1);
    const child = $from.node(depth);
    const before = $from.before(depth);
    if (
      parent.canReplaceWith(index, index + 1, tableType) &&
      child.isTextblock &&
      child.content.size === 0
    ) {
      return { from: before, to: before + child.nodeSize, replacesEmptyBlock: true };
    }
    if (parent.canReplaceWith(index + 1, index + 1, tableType)) {
      return {
        from: before + child.nodeSize,
        to: before + child.nodeSize,
        replacesEmptyBlock: false,
      };
    }
  }
  return null;
}

export function insertMarkdownTable(rows: number, columns: number): Command {
  return (state, dispatch) => {
    const table = createMarkdownTable(state.schema, rows, columns);
    const target = table ? findMarkdownTableInsertionTarget(state) : null;
    if (!table || !target) return false;
    if (!dispatch) return true;
    const tr = state.tr.replaceWith(target.from, target.to, table);
    const firstCellText = Math.min(target.from + 3, tr.doc.content.size);
    tr.setSelection(TextSelection.near(tr.doc.resolve(firstCellText), 1));
    dispatchTableTransaction(dispatch)(tr.scrollIntoView());
    return true;
  };
}

export interface MarkdownTableContext {
  readonly rows: number;
  readonly columns: number;
  readonly canAddRow: boolean;
  readonly canDeleteRow: boolean;
  readonly canAddColumn: boolean;
  readonly canDeleteColumn: boolean;
  /** Exact logical-source span for the selected table, when projection supplied one. */
  readonly sourceRange?: MarkdownTableSourceRange | null;
}

export interface MarkdownTableSourceRange {
  readonly from: number;
  readonly to: number;
}

export function getMarkdownTableContext(state: EditorState): MarkdownTableContext | null {
  const selectedNode =
    state.selection instanceof NodeSelection &&
    state.selection.node?.type.spec.tableRole === "table"
      ? state.selection.node
      : null;
  if (selectedNode) {
    const columns = selectedNode.firstChild?.childCount ?? 0;
    const rows = selectedNode.childCount;
    const sourceRange = sourceRangeFromTable(selectedNode);
    return {
      rows,
      columns,
      canAddRow: rows < MARKDOWN_TABLE_MAX_ROWS,
      canDeleteRow: rows > MARKDOWN_TABLE_MIN_ROWS,
      canAddColumn: columns < MARKDOWN_TABLE_MAX_COLUMNS,
      canDeleteColumn: columns > MARKDOWN_TABLE_MIN_COLUMNS,
      ...(sourceRange ? { sourceRange } : {}),
    };
  }
  if (!isInTable(state)) return null;
  const rect = selectedRect(state);
  const sourceRange = sourceRangeFromTable(rect.table);
  return {
    rows: rect.map.height,
    columns: rect.map.width,
    canAddRow: rect.map.height < MARKDOWN_TABLE_MAX_ROWS,
    canDeleteRow: rect.map.height > MARKDOWN_TABLE_MIN_ROWS,
    canAddColumn: rect.map.width < MARKDOWN_TABLE_MAX_COLUMNS,
    canDeleteColumn: rect.map.width > MARKDOWN_TABLE_MIN_COLUMNS,
    ...(sourceRange ? { sourceRange } : {}),
  };
}

/** Exact logical-source span for source reveal; unavailable for unprojected legacy views. */
export function getMarkdownTableSourceRange(state: EditorState): MarkdownTableSourceRange | null {
  const table =
    state.selection instanceof NodeSelection &&
    state.selection.node?.type.spec.tableRole === "table"
      ? state.selection.node
      : isInTable(state)
        ? selectedRect(state).table
        : null;
  return table ? sourceRangeFromTable(table) : null;
}

function sourceRangeFromTable(table: ProseMirrorNode): MarkdownTableSourceRange | null {
  const from = Number(table.attrs.sourceFrom ?? -1);
  const to = Number(table.attrs.sourceTo ?? -1);
  if (!Number.isInteger(from) || !Number.isInteger(to) || from < 0 || to < from) return null;
  return { from, to };
}

function guardedTableCommand(
  command: Command,
  legal: (context: MarkdownTableContext) => boolean,
): Command {
  return (state, dispatch) => {
    const context = getMarkdownTableContext(state);
    if (!context || !legal(context)) return false;
    if (!dispatch) return command(state);
    const tablePosition = selectedRect(state).tableStart - 1;
    return command(state, (tr) => {
      // GFM always treats first row as header, including after inserting/deleting above it.
      // Keep live cells consistent with saved Markdown & subsequent projection.
      const table = tr.doc.nodeAt(tablePosition);
      table?.forEach((row, rowOffset, rowIndex) => {
        const cellType =
          rowIndex === 0 ? state.schema.nodes.table_header : state.schema.nodes.table_cell;
        if (!cellType) return;
        row.forEach((cell, cellOffset) => {
          if (cell.type !== cellType)
            tr.setNodeMarkup(tablePosition + rowOffset + cellOffset + 2, cellType, cell.attrs);
        });
      });
      dispatchTableTransaction(dispatch)(tr);
    });
  };
}

export const markdownTableAddRowBefore = guardedTableCommand(
  pmAddRowBefore,
  (context) => context.rows < MARKDOWN_TABLE_MAX_ROWS,
);
export const markdownTableAddRowAfter = guardedTableCommand(
  pmAddRowAfter,
  (context) => context.rows < MARKDOWN_TABLE_MAX_ROWS,
);
export const markdownTableAddColumnBefore = guardedColumnCommand(
  pmAddColumnBefore,
  (context) => context.columns < MARKDOWN_TABLE_MAX_COLUMNS,
  (align, left) => [...align.slice(0, left), null, ...align.slice(left)],
);
export const markdownTableAddColumnAfter = guardedColumnCommand(
  pmAddColumnAfter,
  (context) => context.columns < MARKDOWN_TABLE_MAX_COLUMNS,
  (align, _left, right) => [...align.slice(0, right), null, ...align.slice(right)],
);
export const markdownTableDeleteRow = guardedTableCommand(
  pmDeleteRow,
  (context) => context.rows > MARKDOWN_TABLE_MIN_ROWS,
);
export const markdownTableDeleteColumn = guardedColumnCommand(
  pmDeleteColumn,
  (context) => context.columns > MARKDOWN_TABLE_MIN_COLUMNS,
  (align, left, right) => [...align.slice(0, left), ...align.slice(right)],
);

/**
 * ProseMirror's structural column commands correctly create/remove cells but do
 * not know Markdown's table-level alignment vector. Keep it in lock-step so a
 * later serialization or reprojection never shifts a column's meaning.
 */
function guardedColumnCommand(
  command: Command,
  legal: (context: MarkdownTableContext) => boolean,
  adjust: (
    align: readonly MarkdownTableAlignment[],
    left: number,
    right: number,
  ) => MarkdownTableAlignment[],
): Command {
  return (state, dispatch) => {
    const context = getMarkdownTableContext(state);
    if (!context || !legal(context)) return false;
    if (!dispatch) return command(state);
    const rect = selectedRect(state);
    const priorAlign = normalizeTableAlignment(
      rect.table.attrs.align as readonly MarkdownTableAlignment[] | null | undefined,
      rect.map.width,
    );
    return command(state, (tr) => {
      const tablePosition = rect.tableStart - 1;
      const nextTable = tr.doc.nodeAt(tablePosition);
      if (nextTable) {
        tr.setNodeMarkup(tablePosition, undefined, {
          ...nextTable.attrs,
          align: adjust(priorAlign, rect.left, rect.right),
        });
      }
      dispatchTableTransaction(dispatch)(tr);
    });
  };
}
export const markdownTableDelete: Command = (state, dispatch) =>
  pmDeleteTable(state, dispatch ? dispatchTableTransaction(dispatch) : undefined);

export function setMarkdownTableAlignment(alignment: MarkdownTableAlignment): Command {
  return (state, dispatch) => {
    if (!isInTable(state)) return false;
    const rect = selectedRect(state);
    if (!rect.table.type.spec.attrs?.align) return false;
    const align = normalizeTableAlignment(
      rect.table.attrs.align as readonly MarkdownTableAlignment[] | null | undefined,
      rect.map.width,
    );
    for (let column = rect.left; column < rect.right; column++) align[column] = alignment;
    if (dispatch) {
      const tr = state.tr.setNodeMarkup(rect.tableStart - 1, undefined, {
        ...rect.table.attrs,
        align,
      });
      const cellPositions = new Set<number>();
      for (let row = 0; row < rect.map.height; row++) {
        for (let column = rect.left; column < rect.right; column++) {
          const relative = rect.map.map[row * rect.map.width + column];
          if (typeof relative === "number") cellPositions.add(rect.tableStart + relative);
        }
      }
      for (const position of cellPositions) {
        const cell = tr.doc.nodeAt(position);
        if (!cell) continue;
        tr.setNodeMarkup(position, undefined, { ...cell.attrs, align: alignment });
      }
      dispatchTableTransaction(dispatch)(tr);
    }
    return true;
  };
}

function tabCommand(direction: 1 | -1): Command {
  return (state, dispatch) => {
    if (!isInTable(state)) return false;
    if (goToNextCell(direction)(state, dispatch)) return true;
    if (direction < 0) return false;
    const rect = selectedRect(state);
    if (rect.bottom !== rect.map.height || rect.right !== rect.map.width) return false;
    if (!dispatch) return true;
    const tr = state.tr;
    if (rect.bottom >= MARKDOWN_TABLE_MAX_ROWS) return false;
    addRow(tr, rect, rect.bottom);
    const table = tr.doc.nodeAt(rect.tableStart - 1);
    if (!table) return false;
    const map = TableMap.get(table);
    const firstCell = rect.tableStart + map.positionAt(rect.bottom, 0, table);
    tr.setSelection(TextSelection.near(tr.doc.resolve(firstCell + 1), 1));
    dispatchTableTransaction(dispatch)(tr.scrollIntoView());
    return true;
  };
}

export const markdownTableNextCell = tabCommand(1);
export const markdownTablePreviousCell = tabCommand(-1);

export type MarkdownTableGestureIntent =
  | { readonly kind: "cell" }
  | { readonly kind: "table"; readonly tablePos: number };

/**
 * Map a click on table DOM to a semantic intent. `handleClickOn` calls this
 * once per ancestor node of the click target (deepest first is not
 * guaranteed by PM, so callers pass whichever `node`/`nodePos`/`direct`
 * triple they are currently inspecting).
 *
 * - A direct hit on the `table` node itself (not bubbled up from a cell,
 *   e.g. the scroll-wrapper padding around the `<table>` element) is the
 *   "frame" gesture -> whole-table intent. `direct` is PM's own signal for
 *   this: `handleClickOn` reports it `true` only for the node found exactly
 *   at the click's resolved position boundary, which is the table's own
 *   boundary when the click target is the table's own wrapper DOM rather
 *   than a descendant cell.
 * - Anything else (including any click on a cell) is left alone so ordinary
 *   caret placement, `tableEditing()`'s cell-selection drag, and the
 *   existing toolbar continue to own row/column selection.
 */
export function resolveMarkdownTableGestureIntent(
  node: ProseMirrorNode,
  nodePos: number,
  direct: boolean,
  _event: MouseEvent,
): MarkdownTableGestureIntent | null {
  const role = node.type.spec.tableRole;
  if (role === "table") {
    if (!direct) return null;
    return { kind: "table", tablePos: nodePos };
  }
  if (role === "cell" || role === "header_cell") {
    return { kind: "cell" };
  }
  return null;
}

/**
 * Apply a resolved gesture intent as a PM selection. Cell-text clicks
 * return `false` so ProseMirror's own click handling (and
 * `tableEditing()`'s cell-selection drag logic) proceeds unchanged.
 */
function applyMarkdownTableGestureIntent(
  view: EditorView,
  intent: MarkdownTableGestureIntent,
): boolean {
  switch (intent.kind) {
    case "table": {
      const tr = view.state.tr.setSelection(NodeSelection.create(view.state.doc, intent.tablePos));
      view.dispatch(tr.scrollIntoView());
      return true;
    }
    case "cell":
      return false;
  }
}

/**
 * Gesture -> intent resolver for tables. Extends the existing table
 * gesture handling (rather than introducing a parallel plugin family) so
 * clicking the table frame yields a whole-table `NodeSelection`, while
 * ordinary cell-text clicks are left to `tableEditing()`.
 */
function markdownTableGesturePlugin(): Plugin {
  return new ProseMirrorPlugin({
    props: {
      handleClickOn(view, _pos, node, nodePos, event, direct) {
        const intent = resolveMarkdownTableGestureIntent(node, nodePos, direct, event);
        if (!intent) return false;
        return applyMarkdownTableGestureIntent(view, intent);
      },
    },
  });
}

export function markdownTablePlugins(): Plugin[] {
  return [
    keymap({
      Tab: markdownTableNextCell,
      "Shift-Tab": markdownTablePreviousCell,
      Escape: escapeMarkdownTable,
    }),
    markdownTableGesturePlugin(),
    tableEditing({ allowTableNodeSelection: true }),
  ];
}

export const escapeMarkdownTable: Command = (state, dispatch) => {
  if (!isInTable(state)) return false;
  if (!dispatch) return true;
  const $from = state.selection.$from;
  for (let depth = $from.depth; depth > 0; depth--) {
    if ($from.node(depth).type.spec.tableRole === "table") {
      dispatch(state.tr.setSelection(TextSelection.near(state.doc.resolve($from.after(depth)), 1)));
      return true;
    }
  }
  return false;
};
