import type { EditorView } from "prosemirror-view";
import type { Command } from "prosemirror-state";
import { NodeSelection, TextSelection } from "prosemirror-state";
import { Dropdown, Item, Sep } from "@rightkit/document-core/documents/toolbarPrimitives";
import { setMarkdownTransactionIntent } from "../projection/markdownTransactionIntent";
import {
  getMarkdownTableSourceRange,
  markdownTableAddColumnAfter,
  markdownTableAddColumnBefore,
  markdownTableAddRowAfter,
  markdownTableAddRowBefore,
  markdownTableDelete,
  markdownTableDeleteColumn,
  markdownTableDeleteRow,
  setMarkdownTableAlignment,
  type MarkdownTableContext,
} from "./markdownTableCommands";

export interface MarkdownTableToolbarGroupProps {
  readonly context: MarkdownTableContext;
  readonly getEditorView: () => EditorView | null;
  readonly onRevealSource?: (range: { from: number; to: number }) => void;
}

/**
 * Contextual table commands rendered as a fragment INSIDE the single top Markdown toolbar
 * (owned by MarkdownWorkspacePanel), not a second toolbar. Reads/runs everything through
 * `getEditorView()` and the existing markdownTableCommands authority — no separate command
 * or selection model. Contract 14: the supported table's only editing surface is the native
 * PM cells; there is deliberately no "Edit whole table" duplicate-form entry any more.
 */
export function MarkdownTableToolbarGroup({
  context,
  getEditorView,
  onRevealSource,
}: MarkdownTableToolbarGroupProps) {
  const run = (command: Command) => {
    const view = getEditorView();
    if (!view) return;
    if (
      view.state.selection instanceof NodeSelection &&
      view.state.selection.node?.type.spec.tableRole === "table"
    ) {
      // Structural table commands operate from a cell selection. A frame click intentionally
      // leaves a whole-table NodeSelection; promote it to the first cell only when a
      // row/column/alignment command is chosen.
      const firstCell = view.state.selection.from + 2;
      view.dispatch(
        view.state.tr.setSelection(TextSelection.near(view.state.doc.resolve(firstCell))),
      );
    }
    // One toolbar action = one core history entry (contract 14 CE-2A): structural table
    // transactions carry an explicit intent instead of defaulting to "Typing".
    if (
      !command(view.state, (tr) =>
        view.dispatch(setMarkdownTransactionIntent(tr, "table-structural")),
      )
    )
      return;
    view.focus();
  };
  const sourceRange = (() => {
    const view = getEditorView();
    return view ? getMarkdownTableSourceRange(view.state) : null;
  })();

  return (
    <>
      <Dropdown label={`Table ${context.rows}×${context.columns}`} title="Table" minWidth={96}>
        {(close) => (
          <>
            <Item
              disabled={!sourceRange || !onRevealSource}
              title={
                sourceRange ? "Jump to this table in Markdown source" : "No source range available"
              }
              onRun={() => {
                if (sourceRange) onRevealSource?.(sourceRange);
                close();
              }}
            >
              Open exact table source
            </Item>
            <Item
              onRun={() => {
                run(markdownTableDelete);
                close();
              }}
            >
              Delete table
            </Item>
          </>
        )}
      </Dropdown>
      <Dropdown label="Row" title="Row" minWidth={64}>
        {(close) => (
          <>
            <Item
              disabled={!context.canAddRow}
              title={context.canAddRow ? undefined : "Cannot add row: maximum rows reached"}
              onRun={() => {
                run(markdownTableAddRowBefore);
                close();
              }}
            >
              Insert above
            </Item>
            <Item
              disabled={!context.canAddRow}
              title={context.canAddRow ? undefined : "Cannot add row: maximum rows reached"}
              onRun={() => {
                run(markdownTableAddRowAfter);
                close();
              }}
            >
              Insert below
            </Item>
            <Item
              disabled={!context.canDeleteRow}
              title={context.canDeleteRow ? undefined : "Cannot delete row: table has minimum rows"}
              onRun={() => {
                run(markdownTableDeleteRow);
                close();
              }}
            >
              Delete row
            </Item>
          </>
        )}
      </Dropdown>
      <Dropdown label="Column" title="Column" minWidth={72}>
        {(close) => (
          <>
            <Item
              disabled={!context.canAddColumn}
              title={
                context.canAddColumn ? undefined : "Cannot add column: maximum columns reached"
              }
              onRun={() => {
                run(markdownTableAddColumnBefore);
                close();
              }}
            >
              Insert before
            </Item>
            <Item
              disabled={!context.canAddColumn}
              title={
                context.canAddColumn ? undefined : "Cannot add column: maximum columns reached"
              }
              onRun={() => {
                run(markdownTableAddColumnAfter);
                close();
              }}
            >
              Insert after
            </Item>
            <Item
              disabled={!context.canDeleteColumn}
              title={
                context.canDeleteColumn
                  ? undefined
                  : "Cannot delete column: table has minimum columns"
              }
              onRun={() => {
                run(markdownTableDeleteColumn);
                close();
              }}
            >
              Delete column
            </Item>
          </>
        )}
      </Dropdown>
      <Dropdown label="Align" title="Align" minWidth={64}>
        {(close) => (
          <>
            {(["left", "center", "right"] as const).map((alignment) => (
              <Item
                key={alignment}
                onRun={() => {
                  run(setMarkdownTableAlignment(alignment));
                  close();
                }}
              >
                {alignment === "left" ? "Left" : alignment === "center" ? "Center" : "Right"}
              </Item>
            ))}
          </>
        )}
      </Dropdown>
      <Sep />
    </>
  );
}
