import type { RefObject } from "react";
import { Dropdown, Group, Item, Sep, TBtn } from "@rightkit/document-core/documents/toolbarPrimitives";
import { DocumentToolbarShell } from "@rightkit/document-core/documents/DocumentToolbarShell";
import type { BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import type { RichBlockOp, StructuralAction, StructuralSurface } from "./htmlOperationPlanner";
import type { HtmlSafeBlockEditorHandle } from "./HtmlSafeBlockEditor";
import {
  IconDelete,
  IconMore,
  IconMoveDown,
  IconMoveUp,
  IconUnwrap,
  IconWrap,
} from "./htmlToolbarIcons";

// The "+" block inserter — each entry names the tag `insertSiblingPlan` inserts (Gutenberg's "+"
// menu, sans drag-and-drop). "Heading" defaults to h2, matching the markdown block-insert
// convention; "Image" delegates to the existing per-block image flow rather than duplicating it,
// since a bare inserted <img> outside any container is not valid block content on its own.
const INSERT_BLOCK_TYPES: ReadonlyArray<{ label: string; tag: string }> = [
  { label: "Paragraph", tag: "p" },
  { label: "Heading", tag: "h2" },
  { label: "Bulleted list", tag: "ul" },
  { label: "Quote", tag: "blockquote" },
  { label: "Code block", tag: "pre" },
  { label: "Divider", tag: "hr" },
  { label: "Table", tag: "table" },
];

// Must stay identical to the renameable tag set the visual schema can round-trip.
const BLOCK_TYPES = [
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "div",
  "section",
  "article",
  "blockquote",
  "pre",
] as const;

/**
 * Contextual BLOCK bar for an active HTML block session — transform (block type, wrap/unwrap),
 * move (up/down), structure (insert block, table/list row/column/item), and delete/properties.
 * Split out of the single flat strip (findings §2 item 5) so block-shape commands live apart
 * from inline text formatting; see `HtmlRichEditToolbar` for the format bar.
 */
export function HtmlBlockEditToolbar({
  handleRef,
  snapshot,
  onRichOp,
  onStructuralOp,
  canRichOp,
  canStructuralOp,
  onEditSource,
}: {
  handleRef: RefObject<HtmlSafeBlockEditorHandle | null>;
  snapshot: BlockDraftSnapshot;
  onRichOp?: (op: RichBlockOp) => void;
  onStructuralOp?: (surface: StructuralSurface, action: StructuralAction) => void;
  /** Planner-backed applicability supplied by HtmlDocument; never duplicate HTML rules here. */
  canRichOp?: (op: RichBlockOp) => boolean;
  /** Planner-backed structural applicability supplied by HtmlDocument. */
  canStructuralOp?: (surface: StructuralSurface, action: StructuralAction) => boolean;
  /** Discards this session before routing to existing HTML Source surface. */
  onEditSource?: () => void | Promise<void>;
}) {
  const tag = (snapshot.targetTag ?? snapshot.blockKind).replace(/^html:/, "") || "p";
  const renameable = tag !== "li" && tag !== "td" && tag !== "th" && tag !== "tr";
  const tableCell = tag === "td" || tag === "th";
  const listItem = tag === "li";
  const structural = (surface: StructuralSurface, action: StructuralAction) =>
    onStructuralOp?.(surface, action);
  const runRich = (op: RichBlockOp) => onRichOp?.(op);
  const richEnabled = (op: RichBlockOp) => canRichOp?.(op) ?? true;
  const structuralEnabled = (surface: StructuralSurface, action: StructuralAction) =>
    canStructuralOp?.(surface, action) ?? true;
  const runHandle = (fn: (handle: HtmlSafeBlockEditorHandle) => void) => {
    const handle = handleRef.current;
    if (handle) fn(handle);
  };
  const closeAfter = (close: () => void, fn: () => void) => {
    fn();
    close();
  };

  return (
    <DocumentToolbarShell ariaLabel="HTML block edit" variant="scroll" className="vr-capsule-scroll text-xs">
      <span className="shrink-0 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-chrome-xs uppercase tracking-wide text-muted">
        BLOCK
      </span>

      <Dropdown label="+ Insert" title="+ Insert" minWidth={72} panelWidth={168}>
        {(close) => (
          <>
            {INSERT_BLOCK_TYPES.map(({ label, tag: insertTag }) => (
              <Item
                key={insertTag}
                disabled={!richEnabled({ kind: "insertSibling", tag: insertTag, position: "after" })}
                onRun={() =>
                  closeAfter(close, () =>
                    runRich({ kind: "insertSibling", tag: insertTag, position: "after" }),
                  )
                }
              >
                {label}
              </Item>
            ))}
            <Item onRun={() => closeAfter(close, () => runHandle((handle) => handle.requestInsertImage()))}>
              Image
            </Item>
          </>
        )}
      </Dropdown>

      <Sep />

      {renameable && (
        <Dropdown label={`<${tag}>`} title="Block type" minWidth={88} panelWidth={132}>
          {(close) => (
            <>
              {BLOCK_TYPES.map((option) => (
                <Item
                  key={option}
                  active={option === tag}
                  onRun={() =>
                    closeAfter(close, () => runHandle((handle) => handle.changeBlockType(option)))
                  }
                >
                  {`<${option}>`}
                </Item>
              ))}
            </>
          )}
        </Dropdown>
      )}

      <Sep />
      <Group>
        <TBtn
          label={<IconMoveUp />}
          ariaLabel="Move block up"
          title="Move this block before its previous sibling"
          disabled={!richEnabled({ kind: "moveUp" })}
          onRun={() => runRich({ kind: "moveUp" })}
        />
        <TBtn
          label={<IconMoveDown />}
          ariaLabel="Move block down"
          title="Move this block after its next sibling"
          disabled={!richEnabled({ kind: "moveDown" })}
          onRun={() => runRich({ kind: "moveDown" })}
        />
      </Group>

      <Sep />
      {tableCell && (
        <>
          <Dropdown label="Row" title="Row" minWidth={58} panelWidth={164}>
            {(close) => (
              <>
                <Item
                  disabled={!structuralEnabled("table", "insertRow")}
                  onRun={() => closeAfter(close, () => structural("table", "insertRow"))}
                >
                  Insert table row below
                </Item>
                <Item
                  disabled={!structuralEnabled("table", "deleteRow")}
                  onRun={() => closeAfter(close, () => structural("table", "deleteRow"))}
                >
                  Delete table row
                </Item>
              </>
            )}
          </Dropdown>
          <Dropdown label="Column" title="Column" minWidth={70} panelWidth={164}>
            {(close) => (
              <>
                <Item
                  disabled={!structuralEnabled("table", "insertColumn")}
                  onRun={() => closeAfter(close, () => structural("table", "insertColumn"))}
                >
                  Insert table column
                </Item>
                <Item
                  disabled={!structuralEnabled("table", "deleteColumn")}
                  onRun={() => closeAfter(close, () => structural("table", "deleteColumn"))}
                >
                  Delete table column
                </Item>
              </>
            )}
          </Dropdown>
        </>
      )}
      {listItem && (
        <Dropdown label="Item" title="Item" minWidth={58} panelWidth={148}>
          {(close) => (
            <>
              <Item
                disabled={!structuralEnabled("list", "insertItem")}
                onRun={() => closeAfter(close, () => structural("list", "insertItem"))}
              >
                Insert list item
              </Item>
              <Item
                disabled={!structuralEnabled("list", "deleteItem")}
                onRun={() => closeAfter(close, () => structural("list", "deleteItem"))}
              >
                Delete list item
              </Item>
            </>
          )}
        </Dropdown>
      )}
      <Dropdown label={<IconMore />} title="More" minWidth={32} panelWidth={156}>
        {(close) => (
          <>
            <Item
              onRun={() =>
                closeAfter(close, () => runHandle((handle) => handle.requestProperties()))
              }
            >
              Properties
            </Item>
            <Item onRun={() => closeAfter(close, () => void onEditSource?.())}>Edit source</Item>
            {!tableCell && !listItem && (
              <>
                <Item
                  disabled={!richEnabled({ kind: "wrap" })}
                  onRun={() => closeAfter(close, () => runRich({ kind: "wrap" }))}
                >
                  <span className="inline-flex items-center gap-1.5">
                    <IconWrap />
                    Wrap block
                  </span>
                </Item>
                <Item
                  disabled={!richEnabled({ kind: "unwrap" })}
                  onRun={() => closeAfter(close, () => runRich({ kind: "unwrap" }))}
                >
                  <span className="inline-flex items-center gap-1.5">
                    <IconUnwrap />
                    Unwrap block
                  </span>
                </Item>
                <Item
                  disabled={!richEnabled({ kind: "deleteNode" })}
                  onRun={() => closeAfter(close, () => runRich({ kind: "deleteNode" }))}
                >
                  <span className="inline-flex items-center gap-1.5">
                    <IconDelete />
                    Delete block
                  </span>
                </Item>
              </>
            )}
          </>
        )}
      </Dropdown>
    </DocumentToolbarShell>
  );
}
