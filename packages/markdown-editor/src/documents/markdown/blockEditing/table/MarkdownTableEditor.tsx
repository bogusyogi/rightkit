/**
 * Table editor shell.
 *
 * Composes {@link parseMarkdownTable} and {@link stringifyMarkdownTable}
 * to render an editable grid; edits stage in a block draft, and Done
 * commits that draft through the document core. 200×50 and 100 000-code-unit caps enforced per
 * Packet 10 §20.2.
 */

import { useMemo, useState, type ReactNode } from "react";
import {
  parseMarkdownTable,
  stringifyMarkdownTable,
  insertRow,
  deleteRow,
  insertColumn,
  deleteColumn,
  type MarkdownTableModel,
  type TableCell,
  type TableRow,
} from "../../editors/tableModel.js";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore.js";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft.js";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus.js";

export interface MarkdownTableEditorProps {
  readonly core: MarkdownDocumentCore;
  readonly range: { from: number; to: number };
  readonly onClose?: () => void;
}

const MAX_ROWS = 200;
const MAX_COLS = 50;
const MAX_CODE_UNITS = 100_000;

/**
 * Render the table editor. Reads the table from `range`, tracks an
 * internal model, and stages each change into the block draft with
 * `draft.update`. Done commits the draft; Discard drops it.
 */
export function MarkdownTableEditor({ core, range, onClose }: MarkdownTableEditorProps): ReactNode {
  const initial = useMemo(() => {
    const snap = core.snapshot();
    return parseMarkdownTable(snap.logicalSource, range.from, range.to);
  }, [core, range.from, range.to]);

  const [model, setModel] = useState<MarkdownTableModel | null>(initial);
  const draft = useMarkdownBlockDraft(core, "table", range);

  if (!model) {
    return <div data-testid="markdown-table-editor-invalid">Not a valid table.</div>;
  }

  const commit = (next: MarkdownTableModel): void => {
    if (!withinCaps(next)) return;
    const serialized = stringifyMarkdownTable(next);
    if (serialized.length > MAX_CODE_UNITS) return;
    draft.update(serialized);
    setModel(next);
  };

  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };

  const onCellChange = (rowIdx: number, colIdx: number, value: string): void => {
    if (rowIdx < 0) {
      const header: TableRow = {
        cells: model.header.cells.map((cell, index) => (index === colIdx ? { text: value } : cell)),
      };
      commit({ ...model, header });
      return;
    }
    const rows: TableRow[] = model.rows.map((r: TableRow, i: number) => {
      if (i !== rowIdx) return r;
      const cells: TableCell[] = r.cells.map((c: TableCell, j: number) =>
        j === colIdx ? { text: value } : c,
      );
      return { cells };
    });
    commit({ ...model, rows });
  };

  const onAddRow = (): void => {
    if (model.rows.length >= MAX_ROWS) return;
    commit(
      insertRow(model, model.rows.length, {
        cells: model.header.cells.map(() => ({ text: "" })),
      }),
    );
  };

  const onDeleteRow = (rowIdx: number): void => {
    if (model.rows.length <= 1) return;
    commit(deleteRow(model, rowIdx));
  };

  const onAddColumn = (): void => {
    if (model.header.cells.length >= MAX_COLS) return;
    commit(insertColumn(model, model.header.cells.length, null));
  };

  const onDeleteColumn = (colIdx: number): void => {
    if (model.header.cells.length <= 1) return;
    commit(deleteColumn(model, colIdx));
  };

  return (
    <div
      className="vr-markdown-table-editor"
      data-testid="markdown-table-editor"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          void discard();
        }
      }}
    >
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <table>
        <thead>
          <tr>
            {model.header.cells.map((cell, colIdx) => (
              <th key={colIdx}>
                <input
                  type="text"
                  value={cell.text}
                  aria-label={`Header ${colIdx + 1}: ${cell.text || "untitled"}`}
                  onChange={(e) => onCellChange(-1, colIdx, e.target.value)}
                />
                <button
                  type="button"
                  onClick={() => onDeleteColumn(colIdx)}
                  data-testid={`md-table-del-col-${colIdx}`}
                >
                  ×
                </button>
              </th>
            ))}
            <th>
              <button type="button" onClick={onAddColumn} data-testid="md-table-add-col">
                + col
              </button>
            </th>
          </tr>
        </thead>
        <tbody>
          {model.rows.map((row, rowIdx) => (
            <tr key={rowIdx}>
              {row.cells.map((cell, colIdx) => (
                <td key={colIdx}>
                  <input
                    type="text"
                    value={cell.text}
                    aria-label={`Row ${rowIdx + 1}, ${model.header.cells[colIdx]?.text || `Column ${colIdx + 1}`}`}
                    onChange={(e) => onCellChange(rowIdx, colIdx, e.target.value)}
                  />
                </td>
              ))}
              <td>
                <button
                  type="button"
                  onClick={() => onDeleteRow(rowIdx)}
                  data-testid={`md-table-del-row-${rowIdx}`}
                >
                  ×
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div
        className="vr-markdown-table-editor-controls"
        data-testid="markdown-table-editor-controls"
      >
        <button type="button" onClick={onAddRow} data-testid="md-table-add-row">
          + row
        </button>
        <button type="button" onClick={() => void done()} data-testid="md-table-done">
          Done
        </button>
        <button type="button" onClick={() => void discard()} data-testid="md-table-cancel">
          Cancel
        </button>
      </div>
    </div>
  );
}

function withinCaps(model: MarkdownTableModel): boolean {
  if (model.rows.length >= MAX_ROWS) return false;
  if (model.header.cells.length >= MAX_COLS) return false;
  let total = 0;
  for (const row of model.rows) {
    for (const cell of row.cells) {
      total += cell.text.length;
    }
  }
  return total <= MAX_CODE_UNITS;
}
