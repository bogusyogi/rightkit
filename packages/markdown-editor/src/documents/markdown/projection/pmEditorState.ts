/**
 * ProseMirror editor-state construction helpers for the Markdown editor.
 *
 * Pure functions extracted from MarkdownEditor.tsx: document merging that
 * preserves node identities across re-projection, selection capture/restore
 * between PM and logical-source coordinates, keymap/plugin wiring, table
 * context dedupe, and small DOM/search mapping utilities. Nothing here
 * touches React or the editor lifecycle — callers pass the view/doc/core
 * they already hold.
 */

import { baseKeymap } from "prosemirror-commands";
import { keymap } from "prosemirror-keymap";
import type { Node as ProseMirrorNode } from "prosemirror-model";
import {
  AllSelection,
  EditorState,
  Selection,
  TextSelection,
  type Selection as SelectionType,
  type Transaction,
} from "prosemirror-state";
import type { EditorView } from "prosemirror-view";
import type { TextMatch, TextNodeMapping } from "@rightkit/document-core/documents/searchUtils";
import { mapOffsetToDOM } from "@rightkit/document-core/documents/searchUtils";
import { bytesToBase64 } from "@rightkit/document-core/documents/base64";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import type { MarkdownLogicalBookmark } from "../assets/markdownImage.js";
import { normalizeSoftWraps } from "./pmToMarkdown.js";
import { markdownHybridSchema } from "../markdownHybridSchema.js";
import {
  getMarkdownTableContext,
  markdownTablePlugins,
  type MarkdownTableContext,
} from "../tables/markdownTableCommands.js";
import { codeFenceHighlightPlugin } from "../codeFence/codeFenceHighlight.js";
import {
  canMoveMarkdownBlock,
  computeMoveMarkdownBlock,
  topLevelMarkdownBlockIndexAt,
  type MoveMarkdownBlockDirection,
} from "../core/moveMarkdownBlock.js";
import { projectMarkdownToPm } from "./MarkdownProjection.js";
import {
  blockStart,
  logicalSourceToPmPosition,
  pmSelectionToLogicalRange,
  postPmSelectionToCore,
  positionedRawTextLeaves,
} from "./pmSelectionBridge.js";

export function editablePmRangeForMatch(
  view: EditorView,
  mappings: TextNodeMapping[],
  match: TextMatch,
): { from: number; to: number } | null {
  try {
    const start = mapOffsetToDOM(mappings, match.start);
    const end = mapOffsetToDOM(mappings, match.end);
    const from = view.posAtDOM(start.node, start.offset, -1);
    const to = view.posAtDOM(end.node, end.offset, 1);
    if (from >= to) return null;
    const $from = view.state.doc.resolve(from);
    const $to = view.state.doc.resolve(to);
    if (!$from.sameParent($to) || !$from.parent.isTextblock) return null;
    return { from, to };
  } catch {
    return null;
  }
}

export function isBlankDoc(doc: ProseMirrorNode) {
  return (
    doc.childCount === 1 && doc.firstChild?.type.name === "paragraph" && doc.textContent === ""
  );
}

export function captureLogicalBookmark(
  core: MarkdownDocumentCore,
  view: EditorView,
  position?: { x: number; y: number },
): { bookmark: MarkdownLogicalBookmark; release: () => void } {
  const snapshot = core.snapshot();
  let selection: SelectionType = view.state.selection;
  if (position) {
    const point = view.posAtCoords({ left: position.x, top: position.y });
    if (point) {
      try {
        selection = Selection.near(view.state.doc.resolve(point.pos), 1);
      } catch {
        // Keep current selection when native coordinates resolve outside valid PM content.
      }
    }
  }
  const mapped = pmSelectionToLogicalRange(view.state.doc, selection, snapshot);
  const summaries: MarkdownLogicalBookmark["summaries"] = [];
  const unsubscribe = core.subscribe(0, (summary) => {
    if (summary.revision > snapshot.revision) summaries.push(summary);
  });
  return {
    bookmark: {
      from: Math.min(mapped.anchor, mapped.head),
      to: Math.max(mapped.anchor, mapped.head),
      revision: snapshot.revision,
      summaries,
    },
    release: unsubscribe,
  };
}

interface TextblockSelection {
  readonly kind: "textblock";
  readonly childPath: readonly number[];
  readonly offset: number;
}

interface TopLevelSelection {
  readonly kind: "top-level";
  readonly index: number;
  readonly relativeHead: number;
}

export function captureTopLevelSelection(
  state: EditorState,
): TextblockSelection | TopLevelSelection {
  for (let depth = state.selection.$head.depth; depth > 0; depth--) {
    if (!state.selection.$head.node(depth).isTextblock) continue;
    return {
      kind: "textblock",
      childPath: Array.from({ length: depth }, (_, current) =>
        state.selection.$head.index(current),
      ),
      offset: state.selection.$head.parentOffset,
    };
  }
  const index = state.selection.$head.index(0);
  let blockStart = 0;
  for (let current = 0; current < index; current++) blockStart += state.doc.child(current).nodeSize;
  return { kind: "top-level", index, relativeHead: state.selection.head - blockStart };
}

/**
 * Merge a freshly projected doc with the previous view doc, reusing
 * node identities where the v2 source-map attests the block is unchanged.
 * This guarantees at most one projection rebuild per Done while preserving
 * ProseMirror node objects for unaffected blocks, so decorations and
 * NodeViews survive. The projection cache already limits rebuilds to
 * revision changes; the merge limits churn to the affected block(s).
 */
export function mergeDocsPreservingIdentities(
  oldDoc: ProseMirrorNode,
  freshDoc: ProseMirrorNode,
): ProseMirrorNode {
  if (oldDoc.eq(freshDoc)) return oldDoc;
  const oldById = new Map<string, ProseMirrorNode>();
  oldDoc.forEach((node) => {
    const id = String(node.attrs.blockId ?? "");
    if (id) oldById.set(id, node);
  });
  const newChildren: ProseMirrorNode[] = [];
  let allReused = true;
  freshDoc.forEach((node) => {
    const id = String(node.attrs.blockId ?? "");
    const sig = String(node.attrs.rawSignature ?? "");
    const old = id ? oldById.get(id) : null;
    if (
      old &&
      String(old.attrs.rawSignature ?? "") === sig &&
      old.type === node.type &&
      old.eq(node)
    ) {
      newChildren.push(old);
    } else {
      newChildren.push(node);
      allReused = false;
    }
  });
  if (
    allReused &&
    newChildren.length === oldDoc.childCount &&
    newChildren.every((c, i) => c === oldDoc.child(i)) &&
    String(oldDoc.attrs.trailing ?? "") === String(freshDoc.attrs.trailing ?? "")
  ) {
    return oldDoc;
  }
  return freshDoc.type.create(freshDoc.attrs, newChildren, freshDoc.marks);
}

function selectionForDoc(
  doc: ProseMirrorNode,
  core: MarkdownDocumentCore,
  preservedSelection?: TextblockSelection | TopLevelSelection,
  selectionHint?: { anchor: number; head: number } | null,
): Selection | undefined {
  if (selectionHint) {
    const snap = core.snapshot();
    const anchor = logicalSourceToPmPosition(doc, selectionHint.anchor, snap);
    const head = logicalSourceToPmPosition(doc, selectionHint.head, snap);
    return anchor === 0 && head >= doc.content.size
      ? new AllSelection(doc)
      : TextSelection.between(
          doc.resolve(Math.max(0, Math.min(anchor, doc.content.size))),
          doc.resolve(Math.max(0, Math.min(head, doc.content.size))),
        );
  }
  if (preservedSelection?.kind === "textblock") {
    let node: ProseMirrorNode = doc;
    let contentStart = 0;
    for (const requestedIndex of preservedSelection.childPath) {
      if (!node.childCount) break;
      const index = Math.min(requestedIndex, node.childCount - 1);
      for (let current = 0; current < index; current++)
        contentStart += node.child(current).nodeSize;
      node = node.child(index);
      contentStart += 1;
    }
    return TextSelection.near(
      doc.resolve(contentStart + Math.min(preservedSelection.offset, node.content.size)),
      -1,
    );
  }
  if (preservedSelection && doc.childCount > 0) {
    const index = Math.min(preservedSelection.index, doc.childCount - 1);
    let blockStart = 0;
    for (let current = 0; current < index; current++) blockStart += doc.child(current).nodeSize;
    const block = doc.child(index);
    const head =
      blockStart +
      Math.min(Math.max(preservedSelection.relativeHead, 1), Math.max(block.nodeSize - 1, 1));
    return TextSelection.near(doc.resolve(head), -1);
  }
  return undefined;
}

export function createStateFromMergedDoc(
  doc: ProseMirrorNode,
  core: MarkdownDocumentCore,
  preservedSelection?: TextblockSelection | TopLevelSelection,
  selectionHint?: { anchor: number; head: number } | null,
): EditorState {
  const selection = selectionForDoc(doc, core, preservedSelection, selectionHint);
  return EditorState.create({
    doc,
    selection,
    plugins: [
      keymap({
        "Mod-a": selectAllMarkdown,
        "Mod-z": (state) => {
          postPmSelectionToCore(core, state.doc, state.selection, core.snapshot());
          return core.undo() !== null;
        },
        "Mod-y": (state) => {
          postPmSelectionToCore(core, state.doc, state.selection, core.snapshot());
          return core.redo() !== null;
        },
        "Mod-Shift-z": (state) => {
          postPmSelectionToCore(core, state.doc, state.selection, core.snapshot());
          return core.redo() !== null;
        },
        "Alt-ArrowUp": createMoveMarkdownBlockCommand(core, "up"),
        "Alt-ArrowDown": createMoveMarkdownBlockCommand(core, "down"),
      }),
      keymap(baseKeymap),
      ...markdownTablePlugins(),
      codeFenceHighlightPlugin(),
    ],
  });
}

export function createStateFromCore(
  core: MarkdownDocumentCore,
  preservedSelection?: TextblockSelection | TopLevelSelection,
  selectionHint?: { anchor: number; head: number } | null,
): EditorState {
  const doc = projectMarkdownToPm(core, markdownHybridSchema);
  return createStateFromMergedDoc(doc, core, preservedSelection, selectionHint);
}

/**
 * Builds the small PM state used while a core-backed surface is suspended.
 * MarkdownDocumentCore remains authoritative for source, selection, and history;
 * replacing the projected document lets EditorView release its heavy DOM/node views.
 */
export function createSuspendedState(core: MarkdownDocumentCore): EditorState {
  const doc = markdownHybridSchema.topNodeType.createAndFill();
  if (!doc) throw new Error("Markdown hybrid schema could not create suspended document");
  return createStateFromMergedDoc(doc, core);
}

/**
 * Builds a keymap command that moves the top-level block containing the
 * selection up/down by one, via `moveMarkdownBlock`'s single-edit swap.
 * Mirrors the `Mod-z`/`Mod-y` pattern above: it drives the core directly
 * (not a PM transaction), and the core's own subscription re-syncs this
 * view. Selection restore reuses the same `postSelectionHint` +
 * `commitSummary.selectionHint` path every other core-driven edit uses,
 * so the caret follows the moved block exactly like Undo/Redo restore it.
 */
function createMoveMarkdownBlockCommand(
  core: MarkdownDocumentCore,
  direction: MoveMarkdownBlockDirection,
): (state: EditorState) => boolean {
  return (state) => {
    const snapshot = core.snapshot();
    const logical = pmSelectionToLogicalRange(state.doc, state.selection, snapshot);
    const blockIndex = topLevelMarkdownBlockIndexAt(snapshot, logical.head);
    if (blockIndex === null || !canMoveMarkdownBlock(snapshot, blockIndex, direction)) return false;
    const move = computeMoveMarkdownBlock(snapshot, blockIndex, direction);
    if (!move) return false;
    const newOffset = move.mapOffset(logical.head);
    core.postSelectionHint({
      surface: "prosemirror",
      nodeId: null,
      anchor: newOffset,
      head: newOffset,
      assoc: 1,
    });
    core.applyCommand({
      edits: [move.edit],
      expectedRevision: snapshot.revision,
      origin: "hybrid",
      label: "BlockEdit",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    });
    return true;
  };
}

export function getTableContext(view: EditorView | null): MarkdownTableContext | null {
  if (!view) return null;
  return getMarkdownTableContext(view.state);
}

// Structural equality over the semantic table-context fields only. Freshly-created context
// objects with the same rows/columns/applicability are considered equal so they can be deduped.
export function sameTableContext(
  a: MarkdownTableContext | null | undefined,
  b: MarkdownTableContext | null | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.rows === b.rows &&
    a.columns === b.columns &&
    a.canAddRow === b.canAddRow &&
    a.canDeleteRow === b.canDeleteRow &&
    a.canAddColumn === b.canAddColumn &&
    a.canDeleteColumn === b.canDeleteColumn &&
    a.sourceRange?.from === b.sourceRange?.from &&
    a.sourceRange?.to === b.sourceRange?.to
  );
}

export function assetPayloadBase64(source: Uint8Array | string): string {
  if (typeof source !== "string") return bytesToBase64(source);
  const comma = source.indexOf(",");
  return source.startsWith("data:") && comma >= 0 ? source.slice(comma + 1) : source;
}

export function markdownOffsetForSelection(state: EditorState): number {
  const topLevelIndex = state.selection.$head.index(0);
  let offset = 0;
  for (let index = 0; index < topLevelIndex; index++) {
    const block = state.doc.child(index);
    offset += String(block.attrs.prefix ?? "").length + String(block.attrs.rawSource ?? "").length;
  }
  const block = state.doc.child(Math.min(topLevelIndex, state.doc.childCount - 1));
  if (!block) return offset;
  const sourceFrom = Number(block.attrs.sourceFrom ?? -1);
  const sourceTo = Number(block.attrs.sourceTo ?? -1);
  const mapped = sourceOffsetForRenderedLeaf(state, block, topLevelIndex, sourceFrom);
  if (mapped !== null) return mapped;
  return sourceTo >= 0 ? sourceTo : offset + String(block.attrs.prefix ?? "").length;
}

export function selectAllMarkdown(
  state: EditorState,
  dispatch?: (tr: Transaction) => void,
): boolean {
  dispatch?.(state.tr.setSelection(new AllSelection(state.doc)));
  return true;
}

function sourceOffsetForRenderedLeaf(
  state: EditorState,
  block: ProseMirrorNode,
  topLevelIndex: number,
  sourceFrom: number,
): number | null {
  if (sourceFrom < 0) return null;
  const renderedLeaves: { text: string; start: number }[] = [];
  block.descendants((node, position) => {
    if (node.isText) renderedLeaves.push({ text: node.text ?? "", start: position });
  });
  const renderedBlockStart = blockStart(state.doc, topLevelIndex);
  const relativeHead = state.selection.head - renderedBlockStart - 1;
  const ordinal = renderedLeaves.findIndex(
    (leaf) =>
      relativeHead >= leaf.start &&
      (relativeHead < leaf.start + leaf.text.length ||
        (leaf === renderedLeaves[renderedLeaves.length - 1] &&
          relativeHead === leaf.start + leaf.text.length)),
  );
  if (ordinal < 0) return null;
  const rawLeaves = positionedRawTextLeaves(String(block.attrs.rawSource ?? ""));
  const rendered = renderedLeaves[ordinal];
  const raw = rawLeaves[ordinal];
  if (!raw || normalizeSoftWraps(raw.text) !== rendered.text) return null;
  return sourceFrom + raw.from + Math.min(relativeHead - rendered.start, rendered.text.length);
}
