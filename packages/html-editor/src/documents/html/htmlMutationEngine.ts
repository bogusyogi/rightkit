import * as parse5 from "parse5";
import { findLockedRangeViolation } from "./blocks/htmlLockedRanges";
import {
  buildHtmlShellInfo,
  getCachedParseEntry,
  setCachedParseEntry,
  sourceChecksum,
  type HtmlParseNode,
} from "./sourceMap";

/**
 * HTML source-range transaction engine (architecture: HTML-EDITING-ARCHITECTURE-FINAL.md).
 *
 * Canonical truth is always the HTML source string. Every visual mutation is an atomic
 * patch set computed against the pre-edit canonical source, with a mutation boundary
 * (which source characters may change) and a structural-impact boundary (which
 * source-backed structure the HTML parser may reinterpret). Everything outside those
 * boundaries is proven unchanged textually AND structurally; on any doubt the engine
 * fails closed and leaves canonical source untouched.
 *
 * This module is the common mutation authority. No toolbar command, ProseMirror
 * plugin, raw-island editor or structural control gets a private canonical splice
 * once routing goes through `commitHtmlMutation`.
 */

// ── Stage 0 contract types ────────────────────────────────────────────────────

/** Source-string character range, compatible with `String.prototype.slice`. */
export interface HtmlSourceRange {
  start: number;
  end: number;
}

export type HtmlRevision = string | number;

/** One atomic source edit. All ranges use the same pre-edit coordinate space. */
export interface HtmlSourcePatch {
  range: HtmlSourceRange;
  /** Exact pre-edit source slice this patch expects to replace. */
  expectedText: string;
  text: string;
}

/**
 * An atomic, all-or-nothing transaction over one canonical source revision.
 * - every patch is contained in `mutationBoundary`;
 * - `mutationBoundary` is contained in `impactBoundary`;
 * - `impactBoundary` is contained in the editable body.
 */
export interface HtmlMutationPlan {
  /** Operation label (e.g. "replaceText", "renameBlock", "tableInsertColumn"). */
  operation: string;
  /** Coarse stale guard: the canonical revision the planner observed. */
  expectedRevision: HtmlRevision;
  /** Maximum source range whose characters this transaction may rewrite. */
  mutationBoundary: HtmlSourceRange;
  /** Maximum source-backed region whose parsed structure may legitimately change. */
  impactBoundary: HtmlSourceRange;
  /** Non-overlapping patches in pre-edit coordinates. */
  patches: readonly HtmlSourcePatch[];
}

export type HtmlMutationErrorCode =
  | "invalid-plan"
  | "stale-revision"
  | "expected-text-mismatch"
  | "patch-out-of-bounds"
  | "patch-overlap"
  | "boundary-violation"
  | "containment-violation"
  | "parse-damage"
  | "structural-violation"
  | "shell-violation"
  | "locked-violation";

export class HtmlMutationError extends Error {
  readonly code: HtmlMutationErrorCode;

  constructor(code: HtmlMutationErrorCode, message: string) {
    super(message);
    this.name = "HtmlMutationError";
    this.code = code;
  }
}

export interface HtmlMutationCommit {
  /** The new canonical source (only returned after every check passed). */
  source: string;
  /**
   * Exact inverse plan expressed in the *committed* coordinate space, with
   * `expectedRevision` bound to the committed revision. Applying it through
   * `commitHtmlMutation` restores the pre-transaction source exactly.
   */
  inversePlan: HtmlMutationPlan;
}

export interface HtmlMutationOptions {
  /** Revision function (defaults to `sourceChecksum`). Hosts may plug a counter-owned token. */
  revisionOf?: (source: string) => HtmlRevision;
  /** Default true: reject new parse damage (Section 10). */
  parseValidation?: boolean;
  /** Default true: reject structural change outside the impact boundary (Section 9). */
  structuralValidation?: boolean;
  /**
   * Default false: reject new parse damage inside the impact boundary too.
   * Explicit raw-source edits may opt into a deliberately different policy (Section 10).
   */
  allowNewErrorsInsideImpact?: boolean;
  /** Editable body range; defaults to the computed body-inner (Section 11). */
  editableBody?: HtmlSourceRange;
  /**
   * Locked block ranges (pre-edit coordinates) from the block index. Their bytes must survive
   * verbatim unless listed in `lockedExempt` (the deliberate target, e.g. rawReplace/deleteNode).
   */
  lockedRanges?: readonly HtmlSourceRange[];
  lockedExempt?: readonly HtmlSourceRange[];
}

// ── Shared helpers ────────────────────────────────────────────────────────────

function isFiniteOffset(value: number, sourceLength: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= sourceLength;
}

/** Editable body = bytes strictly between <body> and </body> (whole doc when synthesized). */
export function buildHtmlEditableBody(source: string): HtmlSourceRange {
  return {
    start: buildHtmlShellInfo(source).bodyInner.start,
    end: buildHtmlShellInfo(source).bodyInner.end,
  };
}

function namedRange(range: HtmlSourceRange, name: string): string {
  return `${name} ${JSON.stringify(range)}`;
}

// ── Stage 0: plan shape + boundary validation (fail closed) ───────────────────

export function validateHtmlMutationPlan(
  source: string,
  plan: HtmlMutationPlan,
  editableBody: HtmlSourceRange,
): void {
  const { mutationBoundary: mutation, impactBoundary: impact, patches } = plan;
  const invalid = (message: string): never => {
    throw new HtmlMutationError("invalid-plan", message);
  };

  if (!plan.operation) invalid("A transaction needs an operation name.");
  if (!patches || patches.length === 0) invalid("A transaction needs at least one patch.");

  for (const [name, range] of [
    ["mutationBoundary", mutation],
    ["impactBoundary", impact],
  ] as const) {
    if (!isFiniteOffset(range.start, source.length) || !isFiniteOffset(range.end, source.length))
      invalid(`${name} ${JSON.stringify(range)} is outside 0..${source.length}.`);
    if (range.start > range.end) invalid(`${name} is inverted.`);
  }
  if (!(mutation.start >= impact.start && mutation.end <= impact.end))
    invalid(
      `${namedRange(mutation, "mutationBoundary")} must be contained in impactBoundary ${JSON.stringify(impact)}.`,
    );
  if (!(impact.start >= editableBody.start && impact.end <= editableBody.end))
    invalid(
      `${namedRange(impact, "impactBoundary")} must be contained in the editable body ${JSON.stringify(editableBody)}.`,
    );

  const sorted = [...patches].sort((a, b) => a.range.start - b.range.start);
  let cursor = mutation.start; // end of the previously accepted patch
  let previousStart: number | null = null;
  for (const patch of sorted) {
    const { start, end } = patch.range;
    if (!isFiniteOffset(start, source.length) || !isFiniteOffset(end, source.length))
      throw new HtmlMutationError(
        "patch-out-of-bounds",
        `Patch ${JSON.stringify(patch.range)} exceeds source length.`,
      );
    if (start > end) throw new HtmlMutationError("patch-out-of-bounds", "Patch range is inverted.");
    if (start < mutation.start || end > mutation.end)
      throw new HtmlMutationError(
        "boundary-violation",
        `Patch ${JSON.stringify(patch.range)} is outside mutationBoundary ${JSON.stringify(mutation)}.`,
      );
    if (start < cursor || start === previousStart)
      throw new HtmlMutationError(
        "patch-overlap",
        "Patch ranges overlap or share an insertion offset; coalesce them deterministically first.",
      );
    previousStart = start;
    cursor = end;
    if (patch.text === patch.expectedText) continue; // no-op patch, no slice check required
    if (source.slice(start, end) !== patch.expectedText)
      throw new HtmlMutationError(
        "expected-text-mismatch",
        `Patch at ${start}..${end} expects ${JSON.stringify(patch.expectedText)} but source has ${JSON.stringify(source.slice(start, end))}.`,
      );
  }
}

/**
 * Apply a validated patch set. Patches apply from the highest source offset to the
 * lowest so earlier offsets do not drift while the set is applied, and every
 * expected slice is re-verified at application time.
 */
export function applyHtmlPatchSet(source: string, patches: readonly HtmlSourcePatch[]): string {
  let out = source;
  for (const patch of [...patches].sort((a, b) => b.range.start - a.range.start)) {
    if (patch.text === patch.expectedText) continue;
    const actual = out.slice(patch.range.start, patch.range.end);
    if (actual !== patch.expectedText)
      throw new HtmlMutationError(
        "expected-text-mismatch",
        `Patch at ${patch.range.start}..${patch.range.end} expected ${JSON.stringify(patch.expectedText)} but found ${JSON.stringify(actual)}.`,
      );
    out = out.slice(0, patch.range.start) + patch.text + out.slice(patch.range.end);
  }
  return out;
}

// ── Section 8: direct source-containment proof ────────────────────────────────

/**
 * Prove text outside the mutation boundary is byte-identical, independent of any
 * parser or editor:
 *   candidate[0:B.start]        == original[0:B.start]
 *   candidate[newBoundaryEnd:]  == original[B.end:]
 * where newBoundaryEnd = B.end + delta, delta = Σ(replacementLen - removedLen).
 */
export function verifyDirectContainment(
  original: string,
  candidate: string,
  boundary: HtmlSourceRange,
  delta: number,
): void {
  const newBoundaryEnd = boundary.end + delta;
  const headMatches = candidate.slice(0, boundary.start) === original.slice(0, boundary.start);
  const tailMatches = candidate.slice(newBoundaryEnd) === original.slice(boundary.end);
  if (!headMatches || !tailMatches) {
    throw new HtmlMutationError(
      "containment-violation",
      "Source outside the mutation boundary changed; refusing the transaction.",
    );
  }
}

// ── Section 10: parse diagnostics with coordinate translation ─────────────────

interface ParseDiagnostic {
  code: string;
  offset: number;
}

function parseWithDiagnostics(source: string): { tree: HtmlParseNode; errors: ParseDiagnostic[] } {
  const errors: ParseDiagnostic[] = [];
  const tree: HtmlParseNode = parse5.parse(source, {
    sourceCodeLocationInfo: true,
    onParseError(error) {
      errors.push({ code: String(error.code), offset: error.startOffset });
    },
  });
  return { tree, errors };
}

/**
 * Same contract as `parseWithDiagnostics`, but for the pre-edit `source` string only:
 * reuse sourceMap's parse-tree cache (keyed by checksum + exact string match) when it
 * already holds a tree parsed *with* diagnostics captured, instead of reparsing a
 * string sourceMap has already parsed this revision. Only ever call this with the
 * commit's pre-edit `source` — the post-edit `candidate` is a fresh string that
 * legitimately can't be pre-cached and must go through `parseWithDiagnostics` directly.
 */
function parseSourceWithDiagnosticsCached(source: string): { tree: HtmlParseNode; errors: ParseDiagnostic[] } {
  const cached = getCachedParseEntry(source);
  if (cached && cached.errors) {
    // Diagnostics contract only holds when the cached entry was itself produced
    // with onParseError wired up (errors explicitly present, not just a bare tree
    // cached by some other parseHtmlTree() call site).
    return { tree: cached.tree, errors: cached.errors };
  }
  const result = parseWithDiagnostics(source);
  setCachedParseEntry(source, result.tree, result.errors);
  return result;
}

/**
 * Compare before/after diagnostics with coordinate translation (patch length
 * changes move later source positions). Old errors at/after the mutation boundary
 * end shift by the delta. A new error whose position is outside the impact
 * boundary must have existed before; inside the impact boundary it must
 * either have existed before or be permitted by raw-edit policy.
 *
 * Both diagnostic sets are normalised into ONE coordinate space — the post-edit
 * space — before any signature is built or any boundary is compared. Translating
 * before-errors forward AND after-errors backward compared offsets from two
 * different spaces and never matched for any error past the boundary.
 */
function verifyParseDiagnostics(
  beforeErrors: readonly ParseDiagnostic[],
  afterErrors: readonly ParseDiagnostic[],
  boundary: HtmlSourceRange,
  impact: HtmlSourceRange,
  delta: number,
  allowNewErrorsInsideImpact: boolean,
): void {
  // Pre-edit → post-edit: offsets at/after the mutation boundary end shift by delta.
  // impact.start ≤ boundary.start and everything before boundary.start is byte-identical,
  // so impact.start itself does not move.
  const translateBefore = (offset: number): number =>
    offset >= boundary.end ? offset + delta : offset;
  const newBoundaryEnd = boundary.end + delta;
  const newImpactEnd = impact.end + delta;

  const oldAll = new Set<string>();
  const oldOutside = new Set<string>();
  for (const e of beforeErrors) {
    const offset = translateBefore(e.offset);
    const signature = `${e.code}@${offset}`;
    oldAll.add(signature);
    if (offset < impact.start || offset >= newImpactEnd) oldOutside.add(signature);
  }

  for (const e of afterErrors) {
    const offset = e.offset; // already in post-edit space
    const signature = `${e.code}@${offset}`;
    if (offset < impact.start || offset >= newImpactEnd) {
      // Outside the impact boundary: only pre-existing errors may remain.
      if (!oldOutside.has(signature))
        throw new HtmlMutationError(
          "parse-damage",
          `The transaction introduces new parse damage outside its impact boundary: ${signature}.`,
        );
    } else if (!allowNewErrorsInsideImpact) {
      // Inside the impact boundary: pre-existing errors are fine, edit-owned errors
      // inside the mutation boundary are fine, everything else rejects. parse5 reports
      // character-reference errors one past the reference, so a reference that ends
      // exactly at the boundary (offset === newBoundaryEnd) is still edit-owned.
      const insideMutation = offset >= boundary.start && offset <= newBoundaryEnd;
      if (!oldAll.has(signature) && !insideMutation)
        throw new HtmlMutationError(
          "parse-damage",
          `The transaction introduces new parse damage inside its impact boundary: ${signature}.`,
        );
    }
  }
}

// ── Section 9: structural containment ─────────────────────────────────────────

/**
 * Collect identity signatures for every authored, source-backed node (elements,
 * text, comments, doctype) that lies OUTSIDE the impact region or crosses its
 * edge, in the tree's coordinate space (pre-edit: `delta` 0; post-edit: the
 * committed delta).
 *
 * Identity = tag | position | parent-identity, where:
 * - nodes wholly outside the impact keep start|end (stable or delta-translated);
 * - nodes that merely CROSS an impact edge (containers, boundary-adjacent tails)
 *   keep tag|start only — their end may legitimately shift as the impact region
 *   inside them grows or shrinks;
 * - the parent identity comes from the TREE parent (the nearest ancestor in the
 *   parse tree, located or implied), so foster-parenting and implied-container
 *   changes change the key and are rejected.
 *
 * The structural check passes only when the pre/post signature multisets are
 * identical — any node that moved to another parent (foster parenting), changed
 * implied ownership, or whose proximity to a sibling changed, is a rejection.
 */
function collectStructuralSignatures(
  tree: HtmlParseNode,
  impact: HtmlSourceRange,
  delta: number,
): Map<string, number> {
  const out = new Map<string, number>();
  const impactEnd = impact.end + delta;

  const nodeName = (node: HtmlParseNode): string =>
    node.tagName
      ? String(node.tagName).toLowerCase()
      : node.nodeName === "#text"
        ? "#text"
        : node.nodeName === "#comment"
          ? "#comment"
          : node.nodeName === "#documentType"
            ? "#documentType"
            : "";

  const walk = (node: HtmlParseNode, parentTag: string, parentStart: string): void => {
    const loc = node.sourceCodeLocation;
    if (!loc || typeof loc.startOffset !== "number" || typeof loc.endOffset !== "number") {
      // Parser-implied node: children inherit its identity (tag, no source start).
      const impliedTag = nodeName(node);
      for (const child of node.childNodes ?? [])
        walk(child, impliedTag || parentTag, impliedTag ? "" : parentStart);
      return;
    }
    const tag = nodeName(node);
    const start = loc.startOffset;
    const end = loc.endOffset;
    const fullyInside =
      start >= impact.start && end <= impactEnd && start < impactEnd && end > impact.start;
    if (fullyInside) {
      // The transaction owns everything whose authored span lies inside the
      // impact region, including its whole subtree.
      return;
    }
    const beforeImpact = end <= impact.start;
    const afterImpact = start >= impactEnd;
    const crossing = !beforeImpact && !afterImpact; // container / boundary-crossing tail
    if (!tag) {
      for (const child of node.childNodes ?? []) walk(child, parentTag, parentStart);
      return;
    }
    let signedStart = start;
    let signedEnd = end;
    if (afterImpact) {
      signedStart = start - delta;
      signedEnd = end - delta;
    }
    const key = crossing
      ? `${tag}|${signedStart}|${parentTag}|${parentStart}`
      : `${tag}|${signedStart}|${signedEnd}|${parentTag}|${parentStart}`;
    out.set(key, (out.get(key) ?? 0) + 1);
    for (const child of node.childNodes ?? []) walk(child, tag, String(signedStart));
  };
  for (const child of tree.childNodes ?? []) walk(child, "", "");
  return out;
}

function assertIdenticalSignatures(before: Map<string, number>, after: Map<string, number>): void {
  if (before.size !== after.size) {
    throw new HtmlMutationError(
      "structural-violation",
      `Structural containment failed: authored structure outside the impact boundary changed (${before.size} vs ${after.size} outside nodes).`,
    );
  }
  for (const [key, count] of before) {
    if (after.get(key) !== count) {
      throw new HtmlMutationError(
        "structural-violation",
        `Structural containment failed: authored node ${key} changed ownership or position outside the impact boundary.`,
      );
    }
  }
}

// ── Section 11: shell / body invariants ───────────────────────────────────────

export function verifyShellInvariants(original: string, candidate: string, delta: number): void {
  const pre = buildHtmlShellInfo(original);
  const post = buildHtmlShellInfo(candidate);
  const same = (
    a: { start: number; end: number } | null,
    b: { start: number; end: number } | null,
  ): boolean => {
    if (a === null || b === null) return a === b;
    return a.start === b.start && a.end === b.end;
  };
  // Everything before the mutation boundary is byte-identical, so shell open-side
  // ranges must match exactly; close-side ranges shift by the transaction delta.
  if (!same(pre.doctype, post.doctype)) throw shellViolation("doctype moved or changed.");
  if (!same(pre.htmlOpen, post.htmlOpen)) throw shellViolation("<html> open tag moved or changed.");
  if (
    !same(pre.head, post.head) ||
    !same(pre.headOpen, post.headOpen) ||
    !same(pre.headClose, post.headClose)
  )
    throw shellViolation("<head> structure changed.");
  if (pre.bodyOpen) {
    if (!same(pre.bodyOpen, post.bodyOpen))
      throw shellViolation("<body> open tag moved or changed.");
  }
  if (pre.bodyClose) {
    const close = pre.bodyClose;
    const postClose = post.bodyClose;
    if (
      !postClose ||
      postClose.start !== close.start + delta ||
      postClose.end !== close.end + delta
    )
      throw shellViolation("content escaped or entered <body> outside the edit boundary.");
  }
  if (pre.htmlClose) {
    const close = pre.htmlClose;
    const postClose = post.htmlClose;
    if (
      !postClose ||
      postClose.start !== close.start + delta ||
      postClose.end !== close.end + delta
    )
      throw shellViolation("<html> close range changed; source escaped the document shell.");
  }
}

function shellViolation(detail: string): never {
  throw new HtmlMutationError("shell-violation", `Shell/body invariant violated: ${detail}`);
}

// ── Commit: the single mutation authority (Section 16) ────────────────────────

/**
 * Validate and, only when every invariant holds, apply one atomic transaction.
 * On ANY rejection the canonical source is untouched and the caller must not
 * apply the result. Returns the committed source plus the exact inverse plan.
 *
 * Validation order (Section 7):
 *  revision → plan shape → exact slices → containment → parse diagnostics →
 *  structural containment → shell invariants → inverse generation.
 */
export function commitHtmlMutation(
  source: string,
  plan: HtmlMutationPlan,
  options: HtmlMutationOptions = {},
): HtmlMutationCommit {
  const revisionOf = options.revisionOf ?? sourceChecksum;
  const currentRevision = revisionOf(source);
  if (currentRevision !== plan.expectedRevision) {
    throw new HtmlMutationError(
      "stale-revision",
      `Stale revision: plan was built for ${String(plan.expectedRevision)} but current is ${String(currentRevision)}.`,
    );
  }

  const shell = buildHtmlShellInfo(source);
  const editableBody = options.editableBody ?? {
    start: shell.bodyInner.start,
    end: shell.bodyInner.end,
  };
  validateHtmlMutationPlan(source, plan, editableBody);

  const candidate = applyHtmlPatchSet(source, plan.patches);
  const delta = plan.patches.reduce(
    (sum, p) => sum + p.text.length - (p.range.end - p.range.start),
    0,
  );

  // Section 8 — textual containment, directly.
  verifyDirectContainment(source, candidate, plan.mutationBoundary, delta);

  if (options.lockedRanges && options.lockedRanges.length > 0) {
    const violation = findLockedRangeViolation(
      source,
      candidate,
      plan,
      options.lockedRanges,
      options.lockedExempt,
    );
    if (violation) throw new HtmlMutationError("locked-violation", violation);
  }

  // Sections 9 + 10 share one parse of each side: parse5 with location info is the
  // dominant cost of a commit on a large document, so never parse the same string twice.
  const parseValidation = options.parseValidation !== false;
  const structuralValidation = options.structuralValidation !== false;
  if (parseValidation || structuralValidation) {
    const before = parseSourceWithDiagnosticsCached(source);
    const after = parseWithDiagnostics(candidate);

    // Section 10 — parser diagnostics.
    if (parseValidation) {
      verifyParseDiagnostics(
        before.errors,
        after.errors,
        plan.mutationBoundary,
        plan.impactBoundary,
        delta,
        options.allowNewErrorsInsideImpact === true,
      );
    }

    // Section 9 — structural containment.
    if (structuralValidation) {
      assertIdenticalSignatures(
        collectStructuralSignatures(before.tree, plan.impactBoundary, 0),
        collectStructuralSignatures(after.tree, plan.impactBoundary, delta),
      );
    }
  }

  // Section 11 — shell / body invariants.
  verifyShellInvariants(source, candidate, delta);

  const newRevision = revisionOf(candidate);
  return { source: candidate, inversePlan: buildInversePlan(plan, delta, newRevision) };
}

function buildInversePlan(
  plan: HtmlMutationPlan,
  delta: number,
  newRevision: HtmlRevision,
): HtmlMutationPlan {
  // Post-edit coordinates: each patch's start is shifted by the cumulative delta of
  // every patch that precedes it in source order, not by zero. Sort ascending and
  // carry a running offset, otherwise every multi-patch inverse (rename, wrap,
  // table column, list ops) points at drifted text and undo fails closed.
  const ascending = [...plan.patches].sort((a, b) => a.range.start - b.range.start);
  let shift = 0;
  const inversePatches: HtmlSourcePatch[] = ascending.map((patch) => {
    const start = patch.range.start + shift;
    shift += patch.text.length - (patch.range.end - patch.range.start);
    return {
      range: { start, end: start + patch.text.length },
      expectedText: patch.text,
      text: patch.expectedText,
    };
  });
  return {
    operation: `inverse:${plan.operation}`,
    expectedRevision: newRevision,
    mutationBoundary: {
      start: plan.mutationBoundary.start,
      end: plan.mutationBoundary.end + delta,
    },
    impactBoundary: { start: plan.impactBoundary.start, end: plan.impactBoundary.end + delta },
    patches: inversePatches,
  };
}
