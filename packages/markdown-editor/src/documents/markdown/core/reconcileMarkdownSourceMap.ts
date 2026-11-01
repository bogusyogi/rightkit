/**
 * Deterministic source-map reconciliation.
 *
 * Given an old {@link MarkdownSourceMap}, a new logical source, a
 * {@link MarkdownChangeMap} mapping old positions to new positions,
 * and an {@link MarkdownNodeIdFactory} for minting new IDs, this
 * module produces a new source-map whose node IDs are preserved
 * across edits whenever possible.
 *
 * The algorithm runs in five deterministic passes per Packet 02
 * §12.9. Two nodes are considered matched only after they are
 * committed to `assignedOld` and `assignedNew`. Deleted nodes are
 * never reassigned to unrelated new nodes.
 */

import { rangeOf, isValidRange } from "./MarkdownSourceMap";
import type {
  MarkdownSourceMap,
  MarkdownSourceNode,
  MarkdownSourceNodeCandidate,
  MarkdownSourceRange,
} from "./MarkdownSourceMap";
import type { MarkdownNodeId } from "./MarkdownNodeIdFactory";
import { buildMarkdownSourceMapCandidates } from "./buildMarkdownSourceMap";
import GithubSlugger, { slug } from "github-slugger";
import type { RootContent } from "mdast";
import { nodeText } from "../semanticBlocks";
import { parseToTree } from "../parseMarkdown";

/**
 * Inputs for {@link reconcileMarkdownSourceMap}.
 */
export interface MarkdownReconcileOptions {
  readonly oldMap: MarkdownSourceMap;
  readonly newLogicalSource: string;
  readonly newRevision: number;
  /**
   * Change-map describing how positions in the old logical source
   * map to positions in the new logical source.
   */
  readonly changeMap: MarkdownChangeMapLike;
  /**
   * Explicit node IDs the caller wants preserved across the
   * reconciliation. Order matters: the algorithm honors the
   * request order.
   */
  readonly preserveNodeIds?: readonly MarkdownNodeId[];
  readonly ids: MarkdownNodeIdFactoryLike;
}

/**
 * Minimal change-map surface used by reconciliation. The full
 * {@link import("./MarkdownChangeMap").MarkdownChangeMap} satisfies
 * this contract.
 */
export interface MarkdownChangeMapLike {
  mapRange(from: number, to: number): { from: number; to: number; deleted: boolean };
  readonly changes?: readonly {
    fromA: number;
    toA: number;
    fromB: number;
    toB: number;
  }[];
}

/**
 * Minimal ID-factory surface used by reconciliation. The full
 * {@link import("./MarkdownNodeIdFactory").MarkdownNodeIdFactory}
 * satisfies this contract.
 */
export interface MarkdownNodeIdFactoryLike {
  create(): MarkdownNodeId;
}

const KIND_MATCH_THRESHOLD = 300;
const STRUCTURAL_THRESHOLD = 450;
/**
 * How many top-level blocks the local window may walk back from the
 * first changed block to find a clean start (a block preceded by a
 * blank line). Past this bound the edit falls back to the global path.
 */
const MAX_CONTEXT_BLOCKS = 4;
/** Opening of a YAML front-matter fence (only special at offset 0). */
const FRONT_MATTER_FENCE = /^---/;
/** Text that may contain an ATX heading or a setext underline. */
const HEADING_MARKER = /#|^[ \t>]*(?:=+|-+)[ \t]*$/m;
/**
 * List text that may contain a heading: an ATX opener after list markers,
 * indentation or quote markers, or an indented setext underline.
 */
const LIST_HEADING =
  /^[ \t>]*(?:(?:[-*+]|\d{1,9}[.)])[ \t]+[ \t>]*)*#{1,6}(?:[ \t]|$)|^[ \t>]+(?:=+|-+)[ \t]*$/m;

export function reconcileMarkdownSourceMap(options: MarkdownReconcileOptions): MarkdownSourceMap {
  const local = reconcileLocalWindow(options);
  if (local) return local;
  const { oldMap, newLogicalSource, newRevision, changeMap, ids } = options;

  const newCandidates = buildMarkdownSourceMapCandidates(newLogicalSource).candidates;
  const assignedNew = assignNodeIds(
    oldMap.nodes,
    newCandidates,
    changeMap,
    options.preserveNodeIds ?? [],
    ids,
  );

  // Build the final node list.
  const nodes: MarkdownSourceNode[] = [];
  const topLevelStarts: number[] = [];
  let lastTopLevelIndex = -1;
  for (let i = 0; i < newCandidates.length; i++) {
    const candidate = newCandidates[i];
    const id = assignedNew.get(i);
    if (!id) continue;
    const parentId =
      candidate.parentCandidateIndex !== null
        ? (assignedNew.get(candidate.parentCandidateIndex) ?? null)
        : null;
    const range = rangeOf(candidate.range.from, candidate.range.to);
    if (!isValidRange(range, newLogicalSource.length)) {
      continue;
    }
    const node: MarkdownSourceNode = {
      id,
      kind: candidate.kind,
      mdastType: candidate.mdastType,
      parentId,
      topLevelIndex: candidate.topLevelCandidateIndex,
      ordinal: candidate.ordinal,
      depth: candidate.depth,
      headingId: candidate.headingId,
      range,
      prefixRange: rangeOf(candidate.prefixFrom, candidate.prefixTo),
      rawHash: candidate.rawHash,
      semanticKey: candidate.semanticKey,
    };
    nodes.push(node);
    if (node.topLevelIndex !== lastTopLevelIndex) {
      topLevelStarts.push(nodes.length - 1);
      lastTopLevelIndex = node.topLevelIndex;
    }
  }
  topLevelStarts.push(nodes.length);
  return {
    revision: newRevision,
    nodes,
    topLevelStartIndex: topLevelStarts,
    logicalSource: newLogicalSource,
  };
}

/**
 * Runs the five matching passes of Packet 02 §12.9 and returns the
 * candidate-index → node-ID assignment (every candidate receives an ID;
 * unmatched candidates get freshly minted IDs in candidate order).
 *
 * `oldNodes` and `candidates` may be a contiguous subset of a map (the
 * local window path passes only the changed top-level groups); every
 * `parentId` / `parentCandidateIndex` must then resolve inside the subset.
 */
function assignNodeIds(
  oldNodes: readonly MarkdownSourceNode[],
  newCandidates: readonly MarkdownSourceNodeCandidate[],
  changeMap: MarkdownChangeMapLike,
  preserveOrder: readonly MarkdownNodeId[],
  ids: MarkdownNodeIdFactoryLike,
): Map<number, MarkdownNodeId> {
  const assignedOld = new Set<MarkdownNodeId>();
  const assignedNew = new Map<number, MarkdownNodeId>();

  // Pre-build a parent-id index for old nodes.
  const oldParentById = new Map<MarkdownNodeId, MarkdownNodeId | null>();
  for (const node of oldNodes) {
    oldParentById.set(node.id, node.parentId);
  }

  // Pass 1: explicit preserved targets.
  for (const preserveId of preserveOrder) {
    const oldNode = oldNodes.find((n) => n.id === preserveId);
    if (!oldNode) continue;
    const mapped = changeMap.mapRange(oldNode.range.from, oldNode.range.to);
    const parentId = oldParentById.get(preserveId) ?? null;
    const bestIdx = pickBestCandidate(newCandidates, oldNode, mapped, parentId, assignedNew);
    if (bestIdx === null) continue;
    const score = computeScore(
      oldNode,
      newCandidates[bestIdx],
      mapped,
      oldParentMatchesParent(parentId, newCandidates[bestIdx].parentCandidateIndex, assignedNew),
    );
    if (score < KIND_MATCH_THRESHOLD) continue;
    assignedOld.add(preserveId);
    assignedNew.set(bestIdx, preserveId);
  }
  // Pass 2: exact raw fingerprint. Candidates are bucketed by
  // fingerprint so each lookup only visits candidates that can match.
  const fingerprints = new ExactFingerprintIndex(newCandidates, assignedNew);
  for (let i = 0; i < oldNodes.length; i++) {
    const oldNode = oldNodes[i];
    if (assignedOld.has(oldNode.id)) continue;
    const mapped = changeMap.mapRange(oldNode.range.from, oldNode.range.to);
    const bestIdx = fingerprints.take(oldNode, mapped.from);
    if (bestIdx === null) continue;
    assignedOld.add(oldNode.id);
    assignedNew.set(bestIdx, oldNode.id);
  }
  // Pass 3: structural overlap.
  // Top-level pass.
  for (let i = 0; i < oldNodes.length; i++) {
    const oldNode = oldNodes[i];
    if (assignedOld.has(oldNode.id)) continue;
    if (oldNode.parentId !== null) continue;
    const mapped = changeMap.mapRange(oldNode.range.from, oldNode.range.to);
    const bestIdx = pickBestByStructuralOverlap(newCandidates, oldNode, mapped, null, assignedNew);
    if (bestIdx === null) continue;
    const score = computeScore(
      oldNode,
      newCandidates[bestIdx],
      mapped,
      oldParentMatchesParent(
        oldParentById.get(oldNode.id) ?? null,
        newCandidates[bestIdx].parentCandidateIndex,
        assignedNew,
      ),
    );
    if (score < STRUCTURAL_THRESHOLD) continue;
    assignedOld.add(oldNode.id);
    assignedNew.set(bestIdx, oldNode.id);
  }
  // Nested pass: only re-consider nodes whose parent was matched.
  for (let i = 0; i < oldNodes.length; i++) {
    const oldNode = oldNodes[i];
    if (assignedOld.has(oldNode.id)) continue;
    if (oldNode.parentId === null) continue;
    if (!assignedOld.has(oldNode.parentId)) continue;
    const mapped = changeMap.mapRange(oldNode.range.from, oldNode.range.to);
    const bestIdx = pickBestByStructuralOverlap(
      newCandidates,
      oldNode,
      mapped,
      oldNode.parentId,
      assignedNew,
    );
    if (bestIdx === null) continue;
    const score = computeScore(oldNode, newCandidates[bestIdx], mapped, true);
    if (score < STRUCTURAL_THRESHOLD) continue;
    assignedOld.add(oldNode.id);
    assignedNew.set(bestIdx, oldNode.id);
  }

  // Pass 4: order-preserving fingerprint LCS, top-level only.
  const oldTop = oldNodes.filter((n) => n.parentId === null);
  const newTopCandidates = newCandidates
    .map((c, idx) => ({ candidate: c, idx }))
    .filter((entry) => entry.candidate.parentCandidateIndex === null);
  const oldUnmatched = oldTop
    .map((n, idx) => ({ node: n, idx }))
    .filter((entry) => !assignedOld.has(entry.node.id));
  const newUnmatched = newTopCandidates.filter((entry) => !assignedNew.has(entry.idx));
  if (
    oldUnmatched.length <= 500 &&
    newUnmatched.length <= 500 &&
    oldUnmatched.length > 0 &&
    newUnmatched.length > 0
  ) {
    runLcsPass(oldUnmatched, newUnmatched, assignedOld, assignedNew);
  } else if (oldUnmatched.length > 0 && newUnmatched.length > 0) {
    runUniqueFingerprintPass(oldUnmatched, newUnmatched, assignedOld, assignedNew);
  }

  // Pass 5: mint IDs for unmatched candidates.
  for (let i = 0; i < newCandidates.length; i++) {
    if (assignedNew.has(i)) continue;
    assignedNew.set(i, ids.create());
  }
  return assignedNew;
}

/**
 * Local reconciliation for a single contiguous change.
 *
 * Re-parses only a window of top-level blocks instead of the whole
 * document:
 *
 *   [context blocks][changed blocks][sentinel block]
 *
 * - The window starts at the line start of a *context* block that
 *   precedes the first changed block and is itself preceded by a blank
 *   line (or at offset 0). Everything before the window is unchanged
 *   text whose parse cannot depend on the edit, and a blank line plus an
 *   unchanged first line mean the full parser also starts a fresh
 *   top-level block there.
 * - The window ends at the end of the *sentinel* — the first old block
 *   that starts after the change. The window parse must reproduce the
 *   sentinel exactly, starting exactly on its mapped boundary. That
 *   proves the edit did not open a construct that swallows later text
 *   (unclosed fence, HTML block, math block, description-list merge,
 *   etc.) — from the sentinel on, the new document parses exactly like
 *   the old one.
 * - Context blocks must also re-parse exactly as before, which rejects
 *   upward effects (setext underlines, lazy continuation, definition
 *   list merges).
 *
 * Only the changed blocks are re-matched; context and sentinel keep
 * their nodes and every later node is shifted arithmetically.
 *
 * Documents containing `]:` (link, footnote or abbreviation definitions)
 * resolve references document-wide, which a window parse cannot see.
 * Every edit in those documents takes the global path so typing cannot
 * replace resolved links or footnotes with window-local approximations.
 *
 * Returns `null` whenever any precondition fails; the caller then runs
 * the global path.
 */
function reconcileLocalWindow(options: MarkdownReconcileOptions): MarkdownSourceMap | null {
  const { oldMap, newLogicalSource, newRevision, changeMap, ids } = options;
  const changes = changeMap.changes;
  if (changes?.length !== 1) return null;
  const change = changes[0];
  const oldSource = oldMap.logicalSource;
  const delta = change.toB - change.fromB - (change.toA - change.fromA);
  if (change.fromA !== change.fromB || oldSource.length + delta !== newLogicalSource.length) {
    return null;
  }

  const starts = oldMap.topLevelStartIndex;
  const blockCount = starts.length - 1;
  if (blockCount < 1 || starts[blockCount] !== oldMap.nodes.length) return null;
  const tops: MarkdownSourceNode[] = [];
  for (let i = 0; i < blockCount; i++) {
    const node = oldMap.nodes[starts[i]];
    if (!node || node.parentId !== null || node.topLevelIndex !== i) return null;
    tops.push(node);
  }

  // A document that opens with `---` but is not front matter makes the
  // parser skip container blocks (lists, block quotes) document-wide; a
  // window parse cannot reproduce that.
  if (FRONT_MATTER_FENCE.test(oldSource) && tops[0].kind !== "yaml") return null;

  if (oldSource.includes("]:") || newLogicalSource.includes("]:")) return null;
  // `a..b` are the changed top-level blocks: the block containing (or
  // preceding) each end of the change.
  const a = Math.max(0, lastTopStartingAtOrBefore(tops, change.fromA));
  const b = Math.max(a, lastTopStartingAtOrBefore(tops, change.toA));

  // Context: walk back to a block that starts cleanly after a blank line.
  let w0 = 0;
  let windowStart = 0;
  if (a > 0) {
    let found = -1;
    for (let k = a - 1; k >= Math.max(0, a - MAX_CONTEXT_BLOCKS); k--) {
      if (k === 0) {
        found = 0;
        windowStart = 0;
        break;
      }
      const lineStart = cleanWindowStart(oldSource, tops[k - 1].range.to, tops[k].range.from);
      if (lineStart !== null) {
        found = k;
        windowStart = lineStart;
        break;
      }
    }
    if (found < 0) return null;
    w0 = found;
  }
  const sentinel = b + 1 < blockCount ? b + 1 : -1;
  const ordinalBase = tops[w0].ordinal;
  for (let g = w0; g <= (sentinel >= 0 ? sentinel : b); g++) {
    if (tops[g].ordinal !== ordinalBase + (g - w0)) return null;
  }

  const oldWindowEnd = sentinel >= 0 ? tops[sentinel].range.to : oldSource.length;
  const slice = newLogicalSource.slice(windowStart, oldWindowEnd + delta);
  const built = buildMarkdownSourceMapCandidates(slice);
  if (built.parseError) return null;
  const cands = built.candidates;
  // Front matter is decided at offset 0 by searching for its closing
  // fence anywhere in the document (and a failed attempt has the global
  // effect above); a window that ends before that fence cannot see it.
  if (windowStart === 0 && FRONT_MATTER_FENCE.test(slice) && cands[0]?.kind !== "yaml") return null;
  const windowTops: number[] = [];
  for (let i = 0; i < cands.length; i++) {
    const candidate = cands[i];
    if (candidate.parentCandidateIndex !== null) continue;
    const t = windowTops.length;
    if (candidate.topLevelCandidateIndex !== t || candidate.ordinal !== t) return null;
    windowTops.push(i);
  }
  const contextCount = a - w0;
  const sentinelCount = sentinel >= 0 ? 1 : 0;
  const middleTopCount = windowTops.length - contextCount - sentinelCount;
  if (middleTopCount < 0) return null;
  const topGroupEnd = (t: number) => (t + 1 < windowTops.length ? windowTops[t + 1] : cands.length);
  const middleFrom = contextCount < windowTops.length ? windowTops[contextCount] : cands.length;
  const middleTo = sentinel >= 0 ? windowTops[windowTops.length - 1] : cands.length;

  // Context and sentinel must re-parse exactly as they did before.
  const exact = true;
  for (let k = 0; k < contextCount; k++) {
    const g = w0 + k;
    if (
      !sameGroup(oldMap.nodes, starts[g], starts[g + 1], cands, windowTops[k], topGroupEnd(k), {
        windowStart,
        oldShift: 0,
        exact,
      })
    ) {
      return null;
    }
  }
  if (
    sentinel >= 0 &&
    !sameGroup(
      oldMap.nodes,
      starts[sentinel],
      starts[sentinel + 1],
      cands,
      middleTo,
      cands.length,
      { windowStart, oldShift: delta, exact },
    )
  ) {
    return null;
  }

  // Changed blocks: re-match only these.
  const oldMiddle = oldMap.nodes.slice(starts[a], starts[b + 1]);
  const middle: MarkdownSourceNodeCandidate[] = [];
  for (let i = middleFrom; i < middleTo; i++) {
    const candidate = cands[i];
    const isTop = candidate.parentCandidateIndex === null;
    middle.push({
      ...candidate,
      parentCandidateIndex: isTop ? null : (candidate.parentCandidateIndex as number) - middleFrom,
      topLevelCandidateIndex: w0 + candidate.topLevelCandidateIndex,
      ordinal: isTop ? ordinalBase + candidate.ordinal : candidate.ordinal,
      range: rangeOf(candidate.range.from + windowStart, candidate.range.to + windowStart),
      prefixFrom: candidate.prefixFrom + windowStart,
      prefixTo: candidate.prefixTo + windowStart,
    });
  }

  // Heading slugs are deduplicated document-wide (including headings the
  // map does not record, e.g. inside list items), so the heading sequence
  // of the changed blocks decides whether any slug can move.
  const oldHeadings = oldMiddle.filter((node) => node.kind === "heading");
  const newHeadings = middle.filter((node) => node.kind === "heading");
  const recordedHeadingsUnchanged =
    oldHeadings.length === newHeadings.length &&
    oldHeadings.every(
      (node, index) =>
        node.rawHash === newHeadings[index].rawHash &&
        node.semanticKey === newHeadings[index].semanticKey &&
        node.depth === newHeadings[index].depth,
    );
  // `null`: keep every stored slug. Otherwise the new slug of every
  // recorded heading in document order, or `"approximate"` when exact
  // document-wide slugs cannot be proven from this local window.
  let headingIds: string[] | "approximate" | null = recordedHeadingsUnchanged
    ? null
    : "approximate";
  if (exact) {
    const oldMiddleFrom = contextCount > 0 ? tops[a - 1].range.to : windowStart;
    const oldMiddleTo = sentinel >= 0 ? tops[sentinel].range.from : oldSource.length;
    const newMiddleFrom =
      contextCount > 0 ? cands[windowTops[contextCount - 1]].range.to + windowStart : windowStart;
    const newMiddleTo = oldMiddleTo + delta;
    const oldMiddleText = oldSource.slice(oldMiddleFrom, oldMiddleTo);
    const newMiddleText = newLogicalSource.slice(newMiddleFrom, newMiddleTo);
    if (HEADING_MARKER.test(oldMiddleText) || HEADING_MARKER.test(newMiddleText)) {
      const oldTexts = headingTextsIn(
        oldSource.slice(windowStart, oldWindowEnd),
        oldMiddleFrom - windowStart,
        oldMiddleTo - windowStart,
      );
      const newTexts = headingTextsIn(
        slice,
        newMiddleFrom - windowStart,
        newMiddleTo - windowStart,
      );
      const sameSequence =
        oldTexts.length === newTexts.length &&
        oldTexts.every((text, index) => slug(text) === slug(newTexts[index]));
      headingIds =
        recordedHeadingsUnchanged && sameSequence
          ? null
          : newTexts.length === newHeadings.length
            ? (exactHeadingIds(oldMap, starts[a], starts[b + 1], oldTexts, newTexts) ??
              "approximate")
            : "approximate";
    }
  }
  // Hidden list headings & long authored text participate in global slug
  // deduplication. Never publish approximated slugs after ordinary typing.
  if (headingIds === "approximate") return null;

  const assigned = assignNodeIds(oldMiddle, middle, changeMap, options.preserveNodeIds ?? [], ids);

  const blockShift = middleTopCount - (b - a + 1);
  const nodes: MarkdownSourceNode[] = oldMap.nodes.slice(0, starts[a]);
  const topLevelStarts: number[] = starts.slice(0, a);
  let headingCursor = 0;
  for (let i = 0; i < middle.length; i++) {
    const candidate = middle[i];
    const id = assigned.get(i);
    if (!id) return null;
    if (candidate.parentCandidateIndex === null) topLevelStarts.push(nodes.length);
    const headingId =
      candidate.kind === "heading" && headingIds === null
        ? oldHeadings[headingCursor++].headingId
        : candidate.headingId;
    nodes.push({
      id,
      kind: candidate.kind,
      mdastType: candidate.mdastType,
      parentId:
        candidate.parentCandidateIndex === null
          ? null
          : (assigned.get(candidate.parentCandidateIndex) ?? null),
      topLevelIndex: candidate.topLevelCandidateIndex,
      ordinal: candidate.ordinal,
      depth: candidate.depth,
      headingId,
      range: candidate.range,
      prefixRange: rangeOf(candidate.prefixFrom, candidate.prefixTo),
      rawHash: candidate.rawHash,
      semanticKey: candidate.semanticKey,
    });
  }
  const sentinelPrefix =
    sentinel >= 0
      ? rangeOf(cands[middleTo].prefixFrom + windowStart, cands[middleTo].prefixTo + windowStart)
      : null;
  // Ranges after the change shift by `delta`; the rare range reaching
  // back before it (a prefix after a position-less node) maps like the
  // change map does.
  const shiftRange = (range: MarkdownSourceRange) => {
    if (range.from > change.toA) return rangeOf(range.from + delta, range.to + delta);
    const mapped = changeMap.mapRange(range.from, range.to);
    return rangeOf(mapped.from, mapped.to);
  };
  for (let g = b + 1; g < blockCount; g++) {
    topLevelStarts.push(nodes.length);
    for (let j = starts[g]; j < starts[g + 1]; j++) {
      const node = oldMap.nodes[j];
      const isTop = j === starts[g];
      const prefixRange =
        isTop && g === sentinel && sentinelPrefix ? sentinelPrefix : shiftRange(node.prefixRange);
      if (
        delta === 0 &&
        blockShift === 0 &&
        prefixRange.from === node.prefixRange.from &&
        prefixRange.to === node.prefixRange.to
      ) {
        nodes.push(node);
        continue;
      }
      nodes.push({
        ...node,
        topLevelIndex: node.topLevelIndex + blockShift,
        ordinal: isTop ? node.ordinal + blockShift : node.ordinal,
        range: shiftRange(node.range),
        prefixRange,
      });
    }
  }
  topLevelStarts.push(nodes.length);

  return {
    revision: newRevision,
    logicalSource: newLogicalSource,
    topLevelStartIndex: topLevelStarts,
    nodes: headingIds === null ? nodes : assignHeadingIds(nodes, headingIds),
  };
}

/** Replaces the slug of every heading, in document order. */
function assignHeadingIds(
  nodes: readonly MarkdownSourceNode[],
  headingIds: readonly string[],
): MarkdownSourceNode[] {
  let cursor = 0;
  return nodes.map((node) => {
    if (node.kind !== "heading") return node;
    const headingId = headingIds[cursor++];
    return headingId === node.headingId ? node : { ...node, headingId };
  });
}

/**
 * Exact slugs for a heading change inside the changed groups
 * `[middleFrom, middleTo)` of `oldNodes`, or `null` when they cannot be
 * proven locally.
 *
 * Headings outside the change are only known by their semantic key (the
 * first 80 normalized characters), so their slug inputs are
 * approximated from it. The result is trusted only when (a) every
 * heading in the changed blocks is recorded in the map (the caller
 * checks the new side) and no list outside them may hide one, so no
 * unrecorded heading takes part in deduplication, and (b) the same
 * inputs — exact texts for the old changed headings, approximations
 * elsewhere — reproduce every slug stored in the old map.
 */
function exactHeadingIds(
  oldMap: MarkdownSourceMap,
  middleFrom: number,
  middleTo: number,
  oldTexts: readonly string[],
  newTexts: readonly string[],
): string[] | null {
  const oldNodes = oldMap.nodes;
  let oldCount = 0;
  for (let i = middleFrom; i < middleTo; i++) if (oldNodes[i].kind === "heading") oldCount++;
  if (oldCount !== oldTexts.length) return null;
  // Headings inside list items are not recorded but still take part in
  // deduplication; any list outside the change that may hold one makes
  // the outcome unprovable here.
  for (let i = 0; i < oldNodes.length; i++) {
    const node = oldNodes[i];
    if (node.kind !== "list" || node.parentId !== null || (i >= middleFrom && i < middleTo)) {
      continue;
    }
    if (LIST_HEADING.test(oldMap.logicalSource.slice(node.range.from, node.range.to))) return null;
  }
  const replay = new GithubSlugger();
  const next = new GithubSlugger();
  const headingIds: string[] = [];
  let oldCursor = 0;
  for (let i = 0; i < oldNodes.length; i++) {
    if (i === middleFrom) for (const text of newTexts) headingIds.push(next.slug(text));
    const node = oldNodes[i];
    if (node.kind !== "heading") continue;
    const inMiddle = i >= middleFrom && i < middleTo;
    const text = inMiddle ? oldTexts[oldCursor++] : headingSemanticText(node);
    if (replay.slug(text) !== node.headingId) return null;
    if (!inMiddle) headingIds.push(next.slug(text));
  }
  return headingIds;
}

function headingSemanticText(node: { readonly semanticKey: string }): string {
  return node.semanticKey.split("\0").slice(1).join("\0");
}

/**
 * Slug input text of every heading, recorded or not, whose start lies in
 * `[from, to)` of `source`, in document order. `source` is a window that
 * was just parsed (or is about to be), so this is normally a parse-cache
 * hit.
 */
function headingTextsIn(source: string, from: number, to: number): string[] {
  const texts: string[] = [];
  const visit = (nodes: readonly RootContent[]) => {
    for (const node of nodes) {
      const start = node.position?.start.offset;
      if (node.type === "heading" && typeof start === "number" && start >= from && start < to) {
        texts.push(nodeText(node));
      }
      const children = (node as { children?: RootContent[] }).children;
      if (Array.isArray(children)) visit(children);
    }
  };
  visit(parseToTree(source).children);
  return texts;
}

/** Index of the last top-level node starting at or before `pos`, or -1. */
function lastTopStartingAtOrBefore(tops: readonly MarkdownSourceNode[], pos: number): number {
  let lo = 0;
  let hi = tops.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (tops[mid].range.from <= pos) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found;
}

/**
 * Returns the line start of a top-level block when a window may begin
 * there: only indentation precedes the block on its line, a blank line
 * separates it from the previous block, and the line does not look like
 * a front-matter fence (which is only special at offset 0).
 */
function cleanWindowStart(source: string, previousEnd: number, blockStart: number): number | null {
  const lineStart = source.lastIndexOf("\n", blockStart - 1) + 1;
  if (lineStart <= previousEnd) return null;
  for (let i = lineStart; i < blockStart; i++) {
    const code = source.charCodeAt(i);
    if (code !== 0x20 && code !== 0x09) return null;
  }
  if (!/\n[ \t]*\n/.test(source.slice(previousEnd, lineStart))) return null;
  if (FRONT_MATTER_FENCE.test(source.slice(lineStart, lineStart + 3))) return null;
  return lineStart;
}

/**
 * Compares one old top-level group with a re-parsed candidate group.
 * `exact` compares every node's kind, type, range, hashes, keys, depth,
 * ordinal and parent topology; otherwise only the top-level node's kind,
 * type and range.
 */
function sameGroup(
  oldNodes: readonly MarkdownSourceNode[],
  oldFrom: number,
  oldTo: number,
  cands: readonly MarkdownSourceNodeCandidate[],
  candFrom: number,
  candTo: number,
  options: { windowStart: number; oldShift: number; exact: boolean },
): boolean {
  const { windowStart, oldShift, exact } = options;
  const count = exact ? oldTo - oldFrom : 1;
  if (oldTo - oldFrom < 1 || candTo - candFrom < 1) return false;
  if (exact && candTo - candFrom !== count) return false;
  const localIndexById = new Map<MarkdownNodeId, number>();
  for (let i = 0; i < count; i++) {
    const oldNode = oldNodes[oldFrom + i];
    const candidate = cands[candFrom + i];
    localIndexById.set(oldNode.id, i);
    if (oldNode.kind !== candidate.kind || oldNode.mdastType !== candidate.mdastType) return false;
    if (
      oldNode.range.from + oldShift !== candidate.range.from + windowStart ||
      oldNode.range.to + oldShift !== candidate.range.to + windowStart
    ) {
      return false;
    }
    if (!exact) return true;
    if (
      oldNode.rawHash !== candidate.rawHash ||
      oldNode.semanticKey !== candidate.semanticKey ||
      oldNode.depth !== candidate.depth
    ) {
      return false;
    }
    if (i === 0) {
      if (oldNode.parentId !== null || candidate.parentCandidateIndex !== null) return false;
      continue;
    }
    const oldParent = oldNode.parentId === null ? -1 : (localIndexById.get(oldNode.parentId) ?? -2);
    const candParent =
      candidate.parentCandidateIndex === null ? -1 : candidate.parentCandidateIndex - candFrom;
    if (oldParent !== candParent || oldNode.ordinal !== candidate.ordinal) return false;
    if (
      oldNode.prefixRange.from + oldShift !== candidate.prefixFrom + windowStart ||
      oldNode.prefixRange.to + oldShift !== candidate.prefixTo + windowStart
    ) {
      return false;
    }
  }
  return true;
}

/**
 * Pass-2 index: candidates bucketed by `kind\0rawHash\0semanticKey`,
 * each bucket ordered by (range.from, candidate index). `take` returns
 * the unassigned candidate nearest to `mappedFrom`, breaking distance
 * ties by the lowest candidate index — exactly what a linear scan over
 * all candidates in index order with a strict `<` comparison selects —
 * and removes it. Removal uses union-find "next/previous live slot"
 * pointers so a lookup never rescans consumed candidates.
 */
class ExactFingerprintIndex {
  private readonly buckets = new Map<string, FingerprintBucket>();

  constructor(
    candidates: readonly MarkdownSourceNodeCandidate[],
    assigned: ReadonlyMap<number, MarkdownNodeId>,
  ) {
    const grouped = new Map<string, number[]>();
    for (let i = 0; i < candidates.length; i++) {
      const candidate = candidates[i];
      const key = fingerprintKey(candidate.kind, candidate.rawHash, candidate.semanticKey);
      const list = grouped.get(key);
      if (list) list.push(i);
      else grouped.set(key, [i]);
    }
    for (const [key, indices] of grouped) {
      indices.sort((x, y) => candidates[x].range.from - candidates[y].range.from || x - y);
      const bucket = new FingerprintBucket(
        indices,
        indices.map((i) => candidates[i].range.from),
      );
      for (let slot = 0; slot < indices.length; slot++) {
        if (assigned.has(indices[slot])) bucket.remove(slot);
      }
      this.buckets.set(key, bucket);
    }
  }

  take(oldNode: MarkdownSourceNode, mappedFrom: number): number | null {
    const bucket = this.buckets.get(
      fingerprintKey(oldNode.kind, oldNode.rawHash, oldNode.semanticKey),
    );
    return bucket ? bucket.takeNearest(mappedFrom) : null;
  }
}

class FingerprintBucket {
  /** `next[i]`: union-find link towards the first live slot >= i (length = size + 1). */
  private readonly next: Int32Array;
  /** `prev[i + 1]`: union-find link towards the last live slot <= i (slot -1 is `prev[0]`). */
  private readonly prev: Int32Array;

  constructor(
    private readonly indices: readonly number[],
    private readonly froms: readonly number[],
  ) {
    this.next = new Int32Array(indices.length + 1);
    this.prev = new Int32Array(indices.length + 1);
    for (let i = 0; i <= indices.length; i++) {
      this.next[i] = i;
      this.prev[i] = i;
    }
  }

  remove(slot: number): void {
    this.next[slot] = slot + 1;
    this.prev[slot + 1] = slot;
  }

  takeNearest(target: number): number | null {
    const size = this.indices.length;
    const right = this.findNext(lowerBound(this.froms, target));
    let left = this.findPrev(lowerBound(this.froms, target) - 1);
    if (left >= 0) {
      // Among live slots sharing the left distance, the lowest candidate
      // index sits first in the bucket order.
      left = this.findNext(lowerBound(this.froms, this.froms[left]));
    }
    let slot = -1;
    if (left >= 0 && right < size) {
      const leftDistance = target - this.froms[left];
      const rightDistance = this.froms[right] - target;
      if (leftDistance < rightDistance) slot = left;
      else if (rightDistance < leftDistance) slot = right;
      else slot = this.indices[left] < this.indices[right] ? left : right;
    } else if (left >= 0) {
      slot = left;
    } else if (right < size) {
      slot = right;
    }
    if (slot < 0) return null;
    this.remove(slot);
    return this.indices[slot];
  }

  private findNext(slot: number): number {
    let root = slot;
    while (this.next[root] !== root) root = this.next[root];
    let cursor = slot;
    while (this.next[cursor] !== root) {
      const following = this.next[cursor];
      this.next[cursor] = root;
      cursor = following;
    }
    return root;
  }

  private findPrev(slot: number): number {
    let root = slot + 1;
    while (this.prev[root] !== root) root = this.prev[root];
    let cursor = slot + 1;
    while (this.prev[cursor] !== root) {
      const following = this.prev[cursor];
      this.prev[cursor] = root;
      cursor = following;
    }
    return root - 1;
  }
}

function fingerprintKey(kind: string, rawHash: string, semanticKey: string): string {
  return `${kind}\0${rawHash}\0${semanticKey}`;
}

function lowerBound(values: readonly number[], target: number): number {
  let lo = 0;
  let hi = values.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (values[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function pickBestCandidate(
  candidates: readonly MarkdownSourceNodeCandidate[],
  oldNode: MarkdownSourceNode,
  mapped: { from: number; to: number; deleted: boolean },
  parentId: MarkdownNodeId | null,
  assignedNew: Map<number, MarkdownNodeId>,
): number | null {
  let bestIdx: number | null = null;
  let bestScore = Number.NEGATIVE_INFINITY;
  for (let i = 0; i < candidates.length; i++) {
    if (assignedNew.has(i)) continue;
    const candidate = candidates[i];
    const sameParent = oldParentMatchesParent(
      parentId,
      candidate.parentCandidateIndex,
      assignedNew,
    );
    const score = computeScore(oldNode, candidate, mapped, sameParent);
    if (score > bestScore || (score === bestScore && bestIdx !== null && i < bestIdx)) {
      bestScore = score;
      bestIdx = i;
    }
  }
  return bestIdx;
}

function pickBestByStructuralOverlap(
  candidates: readonly MarkdownSourceNodeCandidate[],
  oldNode: MarkdownSourceNode,
  mapped: { from: number; to: number; deleted: boolean },
  constrainParent: MarkdownNodeId | null,
  assignedNew: Map<number, MarkdownNodeId>,
): number | null {
  let bestIdx: number | null = null;
  let bestScore = Number.NEGATIVE_INFINITY;
  for (let i = 0; i < candidates.length; i++) {
    if (assignedNew.has(i)) continue;
    const candidate = candidates[i];
    if (candidate.kind !== oldNode.kind) continue;
    if (constrainParent !== null) {
      const candidateParentId = assignedNew.get(candidate.parentCandidateIndex ?? -1);
      if (candidateParentId !== constrainParent) continue;
    }
    const score = computeScore(oldNode, candidate, mapped, constrainParent !== null);
    if (score > bestScore || (score === bestScore && bestIdx !== null && i < bestIdx)) {
      bestScore = score;
      bestIdx = i;
    }
  }
  return bestIdx;
}

function runLcsPass(
  oldUnmatched: ReadonlyArray<{ node: MarkdownSourceNode; idx: number }>,
  newUnmatched: ReadonlyArray<{ candidate: MarkdownSourceNodeCandidate; idx: number }>,
  assignedOld: Set<MarkdownNodeId>,
  assignedNew: Map<number, MarkdownNodeId>,
): void {
  const dp = buildLcsTable(oldUnmatched, newUnmatched);
  const matches = backtrackLcs(dp, oldUnmatched, newUnmatched);
  for (const { oldIdx, newIdx } of matches) {
    const oldNode = oldUnmatched[oldIdx]?.node;
    const newIdxInFlat = newUnmatched[newIdx]?.idx;
    if (!oldNode || newIdxInFlat === undefined) continue;
    if (assignedOld.has(oldNode.id)) continue;
    if (assignedNew.has(newIdxInFlat)) continue;
    assignedOld.add(oldNode.id);
    assignedNew.set(newIdxInFlat, oldNode.id);
  }
}

function runUniqueFingerprintPass(
  oldUnmatched: ReadonlyArray<{ node: MarkdownSourceNode; idx: number }>,
  newUnmatched: ReadonlyArray<{ candidate: MarkdownSourceNodeCandidate; idx: number }>,
  assignedOld: Set<MarkdownNodeId>,
  assignedNew: Map<number, MarkdownNodeId>,
): void {
  const fingerprintToNew = new Map<
    string,
    { candidate: MarkdownSourceNodeCandidate; idx: number }
  >();
  for (const entry of newUnmatched) {
    if (assignedNew.has(entry.idx)) continue;
    const fp = `${entry.candidate.kind}\0${entry.candidate.rawHash}\0${entry.candidate.semanticKey}`;
    if (!fingerprintToNew.has(fp)) fingerprintToNew.set(fp, entry);
  }
  for (const entry of oldUnmatched) {
    if (assignedOld.has(entry.node.id)) continue;
    const fp = `${entry.node.kind}\0${entry.node.rawHash}\0${entry.node.semanticKey}`;
    const target = fingerprintToNew.get(fp);
    if (!target) continue;
    assignedOld.add(entry.node.id);
    assignedNew.set(target.idx, entry.node.id);
    fingerprintToNew.delete(fp);
  }
}

function buildLcsTable(
  old: ReadonlyArray<{ node: MarkdownSourceNode }>,
  next: ReadonlyArray<{ candidate: MarkdownSourceNodeCandidate }>,
): number[][] {
  const m = old.length;
  const n = next.length;
  const table: number[][] = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (
        sameFingerprint(
          old[i - 1] as { node: MarkdownSourceNode },
          next[j - 1] as { candidate: MarkdownSourceNodeCandidate },
        )
      ) {
        (table[i] as number[])[j] = (table[i - 1]?.[j - 1] ?? 0) + 1;
      } else {
        (table[i] as number[])[j] = Math.max(table[i - 1]?.[j] ?? 0, table[i]?.[j - 1] ?? 0);
      }
    }
  }
  return table;
}

function backtrackLcs(
  table: number[][],
  old: ReadonlyArray<{ node: MarkdownSourceNode }>,
  next: ReadonlyArray<{ candidate: MarkdownSourceNodeCandidate }>,
): Array<{ oldIdx: number; newIdx: number }> {
  const result: Array<{ oldIdx: number; newIdx: number }> = [];
  let i = old.length;
  let j = next.length;
  while (i > 0 && j > 0) {
    if (
      sameFingerprint(
        old[i - 1] as { node: MarkdownSourceNode },
        next[j - 1] as { candidate: MarkdownSourceNodeCandidate },
      )
    ) {
      result.push({ oldIdx: i - 1, newIdx: j - 1 });
      i--;
      j--;
    } else if ((table[i - 1]?.[j] ?? 0) >= (table[i]?.[j - 1] ?? 0)) {
      i--;
    } else {
      j--;
    }
  }
  return result.reverse();
}

interface NodeLike {
  readonly node?: MarkdownSourceNode;
  readonly candidate?: MarkdownSourceNodeCandidate;
}

function sameFingerprint(a: NodeLike, b: NodeLike): boolean {
  if (a.node && b.candidate) {
    return (
      a.node.kind === b.candidate.kind &&
      a.node.rawHash === b.candidate.rawHash &&
      a.node.semanticKey === b.candidate.semanticKey
    );
  }
  return false;
}

function oldParentMatchesParent(
  oldParentId: MarkdownNodeId | null,
  newParentCandidateIndex: number | null,
  assignedNew: ReadonlyMap<number, MarkdownNodeId>,
): boolean {
  if (oldParentId === null && newParentCandidateIndex === null) return true;
  if (oldParentId === null || newParentCandidateIndex === null) return false;
  return assignedNew.get(newParentCandidateIndex) === oldParentId;
}

/**
 * Score a candidate against an old node. Mirrors Packet 02 §12.10
 * exactly. Negative-infinity for mismatched kinds so unrelated
 * candidates never win.
 */
function computeScore(
  oldNode: MarkdownSourceNode,
  candidate: MarkdownSourceNodeCandidate,
  mapped: { from: number; to: number; deleted: boolean },
  sameParent: boolean,
): number {
  if (oldNode.kind !== candidate.kind) return Number.NEGATIVE_INFINITY;
  const overlap = Math.max(
    0,
    Math.min(mapped.to, candidate.range.to) - Math.max(mapped.from, candidate.range.from),
  );
  const union =
    Math.max(mapped.to, candidate.range.to) - Math.min(mapped.from, candidate.range.from);
  const overlapRatio = union > 0 ? overlap / union : 0;
  const distance = Math.abs(mapped.from - candidate.range.from);
  let score = 0;
  if (sameParent) score += 250;
  if (oldNode.rawHash === candidate.rawHash) score += 500;
  if (oldNode.semanticKey === candidate.semanticKey) score += 220;
  score += Math.round(overlapRatio * 300);
  score += Math.max(0, 128 - Math.min(128, distance));
  if (oldNode.ordinal === candidate.ordinal) score += 40;
  if (mapped.deleted) score -= 200;
  return score;
}
