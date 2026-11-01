/**
 * Selection bridge between a PM doc and the Markdown core.
 *
 * Maps a PM selection's anchor/head to logical-source offsets using
 * nested-node traversal: top-level block source-map ranges plus
 * text-leaf mappings for intra-block offsets. Handles atom inlines,
 * raw_blocks as atomic, multi-block selections by block boundaries,
 * and trailing document ranges without serializing the document.
 */

import type { Node as ProseMirrorNode } from "prosemirror-model";
import { AllSelection, NodeSelection, type Selection } from "prosemirror-state";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import type { MarkdownSelectionHint } from "../core/MarkdownDocumentTypes.js";
import { parseToTree } from "../parseMarkdown.js";
import { normalizeSoftWraps } from "./pmToMarkdown.js";

// ---------------------------------------------------------------------------
// Helpers: raw text leaves
// ---------------------------------------------------------------------------

export function positionedRawTextLeaves(source: string): { text: string; from: number }[] {
  try {
    const leaves: { text: string; from: number }[] = [];
    const visit = (node: {
      type?: string;
      value?: unknown;
      position?: { start?: { offset?: unknown }; end?: { offset?: unknown } };
      children?: unknown;
    }) => {
      if (typeof node.value === "string" && typeof node.position?.start?.offset === "number") {
        let from = node.position.start.offset;
        if (node.type === "inlineCode") {
          const delimiter = /^`+/.exec(source.slice(from))?.[0].length ?? 0;
          const end = node.position.end?.offset;
          const body =
            typeof end === "number" ? source.slice(from + delimiter, end - delimiter) : "";
          const padded = body.startsWith(" ") && body.endsWith(" ") && body.trim() !== "";
          from += delimiter + (padded ? 1 : 0);
        }
        leaves.push({ text: node.value, from });
      }
      if (Array.isArray(node.children))
        (node.children as unknown[]).forEach((child) => visit(child as never));
    };
    visit(parseToTree(source) as never);
    return leaves;
  } catch {
    return [];
  }
}

export function blockStart(doc: ProseMirrorNode, index: number): number {
  let start = 0;
  for (let i = 0; i < index; i++) start += doc.child(i).nodeSize;
  return start;
}

function topLevelIndexForPos(doc: ProseMirrorNode, pos: number): number {
  // Clamp pos into valid range
  const clamped = Math.max(0, Math.min(pos, doc.content.size));
  try {
    const $pos = doc.resolve(clamped);
    // For pos at very end (doc.content.size) the resolved depth may be 0
    // Use index(0) when inside doc, else last index
    if ($pos.depth === 0 && clamped === doc.content.size && doc.childCount > 0) {
      return doc.childCount - 1;
    }
    return $pos.index(0);
  } catch {
    // Fallback: linear scan
    let acc = 0;
    for (let i = 0; i < doc.childCount; i++) {
      const node = doc.child(i);
      if (clamped >= acc && clamped <= acc + node.nodeSize) return i;
      acc += node.nodeSize;
    }
    return Math.max(0, doc.childCount - 1);
  }
}

function isAtomBlock(node: ProseMirrorNode): boolean {
  return Boolean(node.type.spec.atom);
}

function isMultiBlockSelection(doc: ProseMirrorNode, selection: Selection): boolean {
  if (selection instanceof AllSelection) return true;
  if (selection instanceof NodeSelection) {
    // NodeSelection on a top-level atom block is not multi-block
    return false;
  }
  try {
    const anchorIdx = doc.resolve(selection.anchor).index(0);
    const headIdx = doc.resolve(selection.head).index(0);
    return anchorIdx !== headIdx;
  } catch {
    return false;
  }
}

function collectRenderedLeaves(block: ProseMirrorNode): { text: string; start: number }[] {
  const out: { text: string; start: number }[] = [];
  block.descendants((node, pos) => {
    if (node.isText) out.push({ text: node.text ?? "", start: pos });
    // Do not descend into atom blocks (they have no text)
    return !isAtomBlock(node);
  });
  return out;
}

// ---------------------------------------------------------------------------
// Single position mapping
// ---------------------------------------------------------------------------

export function pmPositionToLogicalSource(
  doc: ProseMirrorNode,
  pos: number,
  snapshot?: { logicalSource: string } | null,
): { offset: number; blockId: string | null } {
  const logicalLength = snapshot?.logicalSource.length ?? null;
  // Handle empty doc
  if (doc.childCount === 0) {
    return { offset: 0, blockId: null };
  }
  // End-of-document trailing range: pos at or beyond doc end maps to logicalSource length
  if (pos >= doc.content.size) {
    if (logicalLength !== null) {
      const last = doc.child(doc.childCount - 1);
      const bid = String(last.attrs.blockId ?? "") || null;
      return { offset: logicalLength, blockId: bid };
    }
    // Fallback without snapshot: use last block's sourceTo + trailing
    const last = doc.child(doc.childCount - 1);
    const sourceTo = Number(last.attrs.sourceTo ?? -1);
    const trailing = String(doc.attrs.trailing ?? "");
    if (sourceTo >= 0) {
      return {
        offset: sourceTo + trailing.length,
        blockId: String(last.attrs.blockId ?? "") || null,
      };
    }
    return { offset: 0, blockId: null };
  }
  if (pos < 0) pos = 0;

  const index = topLevelIndexForPos(doc, pos);
  const bStart = blockStart(doc, index);
  const block = doc.child(index);
  if (!block) return { offset: 0, blockId: null };
  const blockId = String(block.attrs.blockId ?? "") || null;
  const sourceFrom = Number(block.attrs.sourceFrom ?? -1);
  const sourceTo = Number(block.attrs.sourceTo ?? -1);

  // Atom block: atomic selection — any interior maps to block boundaries
  if (isAtomBlock(block)) {
    // NodeSelection on this block: anchor at bStart, head at bStart+nodeSize
    // For caret before block -> sourceFrom, after -> sourceTo
    if (pos <= bStart + 1) {
      return { offset: sourceFrom >= 0 ? sourceFrom : 0, blockId };
    }
    if (pos >= bStart + block.nodeSize - 1) {
      if (sourceTo >= 0) return { offset: sourceTo, blockId };
      if (logicalLength !== null) return { offset: logicalLength, blockId };
      return { offset: sourceFrom >= 0 ? sourceFrom : 0, blockId };
    }
    // Interior of atom (should not happen) -> snap to start
    return { offset: sourceFrom >= 0 ? sourceFrom : 0, blockId };
  }

  if (sourceFrom < 0 || sourceTo < 0) {
    // No v2 attrs: fallback to prefix/rough mapping but never serialize whole doc
    if (logicalLength !== null && index === doc.childCount - 1 && pos === doc.content.size - 1) {
      return { offset: logicalLength, blockId };
    }
    return { offset: Math.max(0, pos - bStart - 1), blockId };
  }

  // Try precise leaf mapping
  const renderedLeaves = collectRenderedLeaves(block);
  const rawLeaves = positionedRawTextLeaves(String(block.attrs.rawSource ?? ""));
  const relative = pos - bStart - 1;

  // Before first leaf -> start of block content
  if (renderedLeaves.length === 0) {
    // No text (empty paragraph or container with only nested blocks but no direct text?)
    // For container blocks like bullet_list, renderedLeaves still collected via descendants,
    // so if empty, map to sourceFrom
    if (relative <= 0) return { offset: sourceFrom, blockId };
    return { offset: sourceTo, blockId };
  }

  // Check if relative is before first leaf
  if (relative < renderedLeaves[0].start) {
    return { offset: sourceFrom, blockId };
  }
  // After last leaf
  const lastLeaf = renderedLeaves[renderedLeaves.length - 1];
  if (relative > lastLeaf.start + lastLeaf.text.length) {
    // If at block end, map to sourceTo
    return { offset: sourceTo, blockId };
  }
  if (relative === lastLeaf.start + lastLeaf.text.length) {
    // At end of last leaf -> map to end of its raw range
    const ordinal = renderedLeaves.length - 1;
    const raw = rawLeaves[ordinal];
    const rendered = lastLeaf;
    if (raw && normalizeSoftWraps(raw.text) === rendered.text) {
      return { offset: sourceFrom + raw.from + rendered.text.length, blockId };
    }
    return { offset: sourceTo, blockId };
  }

  const ordinal = renderedLeaves.findIndex(
    (leaf) => relative >= leaf.start && relative < leaf.start + leaf.text.length,
  );
  if (ordinal >= 0) {
    const rendered = renderedLeaves[ordinal];
    const raw = rawLeaves[ordinal];
    if (raw && normalizeSoftWraps(raw.text) === rendered.text) {
      const intra = relative - rendered.start;
      const clamped = Math.min(intra, rendered.text.length);
      return { offset: sourceFrom + raw.from + clamped, blockId };
    }
  }

  // Gap between leaves (e.g., between text nodes separated by marks or after atom inline)
  // Find nearest leaf and interpolate, or fallback to clamped block offset
  // For gaps, find the leaf before the position
  let beforeIdx = -1;
  for (let i = 0; i < renderedLeaves.length; i++) {
    if (relative > renderedLeaves[i].start + renderedLeaves[i].text.length) beforeIdx = i;
  }
  if (beforeIdx >= 0) {
    const rendered = renderedLeaves[beforeIdx];
    const raw = rawLeaves[beforeIdx];
    if (raw && normalizeSoftWraps(raw.text) === rendered.text) {
      // Map gap to end of previous leaf's raw range
      return { offset: sourceFrom + raw.from + rendered.text.length, blockId };
    }
  }

  // Fallback: clamp relative offset to block's source length
  const blockLen = sourceTo - sourceFrom;
  const clamped = Math.max(0, Math.min(relative, blockLen));
  return { offset: sourceFrom + clamped, blockId };
}

export function logicalSourceToPmPosition(
  doc: ProseMirrorNode,
  offset: number,
  snapshot?: { logicalSource: string } | null,
): number {
  const logicalLength = snapshot?.logicalSource.length ?? null;
  if (doc.childCount === 0) return 0;
  if (logicalLength !== null && offset >= logicalLength) return doc.content.size;
  if (offset < 0) offset = 0;

  // Check trailing: if offset beyond last block's sourceTo
  // Find block that contains offset
  let foundBlockIndex = -1;
  let foundBlock: ProseMirrorNode | null = null;
  let foundBStart = 0;
  let foundSourceFrom = -1;
  let foundSourceTo = -1;

  for (let i = 0; i < doc.childCount; i++) {
    const bStart = blockStart(doc, i);
    const block = doc.child(i);
    const sFrom = Number(block.attrs.sourceFrom ?? -1);
    const sTo = Number(block.attrs.sourceTo ?? -1);
    const pFrom = Number(block.attrs.prefixFrom ?? -1);
    const pTo = Number(block.attrs.prefixTo ?? -1);
    // Prefix range maps to position before block
    if (pFrom >= 0 && pTo >= 0 && offset >= pFrom && offset < pTo) {
      return bStart;
    }
    if (sFrom >= 0 && sTo >= 0 && offset >= sFrom && offset < sTo) {
      foundBlockIndex = i;
      foundBlock = block;
      foundBStart = bStart;
      foundSourceFrom = sFrom;
      foundSourceTo = sTo;
      break;
    }
    // Also handle case where offset is exactly at sourceTo (boundary between block and next prefix)
    if (sTo >= 0 && offset === sTo) {
      // At block end -> position at end of block content (before closing)
      const blockEnd = bStart + block.nodeSize - 1;
      // Prefer next block's prefix if exists? But boundary is ambiguous.
      // Return end of this block.
      return blockEnd;
    }
  }

  if (foundBlock && foundBlockIndex >= 0) {
    // Atom block: any offset inside maps to atom position
    if (isAtomBlock(foundBlock)) {
      // For selection inside raw_block, map to its start
      // If offset is at start, position before atom, else after?
      // Simplify: return start+1 for interior
      return foundBStart + 1;
    }

    // Reverse leaf mapping
    const renderedLeaves = collectRenderedLeaves(foundBlock);
    const rawLeaves = positionedRawTextLeaves(String(foundBlock.attrs.rawSource ?? ""));
    if (renderedLeaves.length && rawLeaves.length) {
      for (let ordinal = 0; ordinal < renderedLeaves.length; ordinal++) {
        const raw = rawLeaves[ordinal];
        const rendered = renderedLeaves[ordinal];
        if (!raw || !rendered) continue;
        if (normalizeSoftWraps(raw.text) !== rendered.text) continue;
        const rawStart = foundSourceFrom + raw.from;
        const rawEnd = rawStart + rendered.text.length;
        if (offset >= rawStart && offset <= rawEnd) {
          const intra = offset - rawStart;
          return foundBStart + 1 + rendered.start + Math.min(intra, rendered.text.length);
        }
        // Gap after this leaf
        if (ordinal + 1 < rawLeaves.length) {
          const nextRaw = rawLeaves[ordinal + 1];
          if (nextRaw) {
            const nextRawStart = foundSourceFrom + nextRaw.from;
            if (offset > rawEnd && offset < nextRawStart) {
              // In markup between leaves (e.g., "**" markers) -> map to boundary between leaves
              return foundBStart + 1 + rendered.start + rendered.text.length;
            }
          }
        }
      }
      // If offset is after last leaf but before block end (e.g., trailing markup)
      const lastRendered = renderedLeaves[renderedLeaves.length - 1];
      const lastRaw = rawLeaves[renderedLeaves.length - 1];
      if (lastRendered && lastRaw && normalizeSoftWraps(lastRaw.text) === lastRendered.text) {
        const lastRawEnd = foundSourceFrom + lastRaw.from + lastRendered.text.length;
        if (offset > lastRawEnd && offset < foundSourceTo) {
          return foundBStart + 1 + lastRendered.start + lastRendered.text.length;
        }
      }
    }

    // Fallback: offset relative to block start
    const intra = offset - foundSourceFrom;
    const clamped = Math.max(0, Math.min(intra, foundBlock.nodeSize - 2));
    return foundBStart + 1 + clamped;
  }

  // Not in any block's source range: could be inside trailing or before first block's prefix
  if (doc.childCount > 0) {
    const first = doc.child(0);
    const firstFrom = Number(first.attrs.sourceFrom ?? -1);
    if (firstFrom >= 0 && offset < firstFrom) return 0;
    const last = doc.child(doc.childCount - 1);
    const lastTo = Number(last.attrs.sourceTo ?? -1);
    if (lastTo >= 0 && offset >= lastTo) {
      if (logicalLength !== null) return doc.content.size;
      return blockStart(doc, doc.childCount - 1) + last.nodeSize;
    }
  }

  return 0;
}

// ---------------------------------------------------------------------------
// Multi-block and selection helpers
// ---------------------------------------------------------------------------

/**
 * Map a PM Selection (anchor/head) to logical-source offsets preserving
 * direction, multi-block, and atom semantics.
 */
export function pmSelectionToLogicalRange(
  doc: ProseMirrorNode,
  selection: Selection,
  snapshot?: { logicalSource: string } | null,
): {
  anchor: number;
  head: number;
  assoc: -1 | 1;
  nodeId: string | null;
  direction: "forward" | "backward";
  isMultiBlock: boolean;
  isAtom: boolean;
} {
  const isMulti = isMultiBlockSelection(doc, selection);
  const isAtom = (() => {
    if (selection instanceof NodeSelection) return Boolean(selection.node.type.spec.atom);
    // Check anchor/head inside atom block or atom inline
    try {
      const $a = doc.resolve(selection.anchor);
      const aBlockIdx = $a.index(0);
      const aBlock = doc.child(aBlockIdx);
      if (aBlock && isAtomBlock(aBlock)) return true;
      const nodeAfter = $a.nodeAfter;
      const nodeBefore = $a.nodeBefore;
      if (nodeAfter && nodeAfter.type.spec.atom && nodeAfter.isInline) return true;
      if (nodeBefore && nodeBefore.type.spec.atom && nodeBefore.isInline) return true;
    } catch {
      // ignore
    }
    return false;
  })();

  const direction: "forward" | "backward" =
    selection.anchor <= selection.head ? "forward" : "backward";

  // AllSelection maps to whole document
  if (selection instanceof AllSelection) {
    const logicalLen =
      snapshot?.logicalSource.length ??
      (() => {
        const last = doc.childCount ? doc.child(doc.childCount - 1) : null;
        const sTo = last ? Number(last.attrs.sourceTo ?? -1) : -1;
        const trailing = String(doc.attrs.trailing ?? "");
        return sTo >= 0 ? sTo + trailing.length : doc.content.size;
      })();
    const firstId = doc.childCount ? String(doc.child(0).attrs.blockId ?? "") || null : null;
    return {
      anchor: 0,
      head: logicalLen,
      assoc: -1,
      nodeId: firstId,
      direction: "forward",
      isMultiBlock: true,
      isAtom: false,
    };
  }

  if (isMulti) {
    // Define by block boundaries per spec
    const aIdx = topLevelIndexForPos(doc, selection.anchor);
    const hIdx = topLevelIndexForPos(doc, selection.head);
    const aBlock = doc.child(aIdx);
    const hBlock = doc.child(hIdx);
    const aFrom = Number(aBlock?.attrs.sourceFrom ?? -1);
    const hTo = Number(hBlock?.attrs.sourceTo ?? -1);
    const aId = aBlock ? String(aBlock.attrs.blockId ?? "") || null : null;
    const hId = hBlock ? String(hBlock.attrs.blockId ?? "") || null : null;
    // For reversed selection, anchor is after head, preserve direction
    if (direction === "forward") {
      const anchorOff =
        aFrom >= 0 ? aFrom : pmPositionToLogicalSource(doc, selection.anchor, snapshot).offset;
      const headOff =
        hTo >= 0 ? hTo : pmPositionToLogicalSource(doc, selection.head, snapshot).offset;
      return {
        anchor: anchorOff,
        head: headOff,
        assoc: -1,
        nodeId: aId,
        direction,
        isMultiBlock: true,
        isAtom,
      };
    } else {
      // Actually for backward, anchor is later, head is earlier
      const anchorBlockTo = Number(
        doc.child(topLevelIndexForPos(doc, selection.anchor))?.attrs.sourceTo ?? -1,
      );
      const headBlockFrom = Number(
        doc.child(topLevelIndexForPos(doc, selection.head))?.attrs.sourceFrom ?? -1,
      );
      const aOff =
        anchorBlockTo >= 0
          ? anchorBlockTo
          : pmPositionToLogicalSource(doc, selection.anchor, snapshot).offset;
      const hOff =
        headBlockFrom >= 0
          ? headBlockFrom
          : pmPositionToLogicalSource(doc, selection.head, snapshot).offset;
      return {
        anchor: aOff,
        head: hOff,
        assoc: 1,
        nodeId: hId,
        direction,
        isMultiBlock: true,
        isAtom,
      };
    }
  }

  // Single-block: map each endpoint precisely via leaf mapping
  const aMapped = pmPositionToLogicalSource(doc, selection.anchor, snapshot);
  const hMapped = pmPositionToLogicalSource(doc, selection.head, snapshot);
  // Preserve node identity from head or anchor depending on direction? Use head's block
  const nodeId = hMapped.blockId ?? aMapped.blockId;
  // Assoc: if head after anchor and at block boundary, assoc may be 1
  const assoc: -1 | 1 = direction === "forward" ? -1 : 1;
  return {
    anchor: aMapped.offset,
    head: hMapped.offset,
    assoc,
    nodeId,
    direction,
    isMultiBlock: false,
    isAtom,
  };
}

/**
 * Build and post a {@link MarkdownSelectionHint} on `core` for the
 * supplied PM selection. Preserves anchor/head/direction and node identity.
 */
export function postPmSelectionToCore(
  core: MarkdownDocumentCore,
  doc: ProseMirrorNode,
  selection: Selection,
  snapshot?: { logicalSource: string } | null,
): MarkdownSelectionHint | null {
  const snap =
    snapshot ??
    (() => {
      try {
        return core.snapshot();
      } catch {
        return null;
      }
    })();
  const mapped = pmSelectionToLogicalRange(doc, selection, snap);
  const hint: MarkdownSelectionHint = {
    surface: "prosemirror",
    nodeId: mapped.nodeId as MarkdownSelectionHint["nodeId"],
    anchor: mapped.anchor,
    head: mapped.head,
    assoc: mapped.assoc,
  };
  core.postSelectionHint(hint);
  return hint;
}
