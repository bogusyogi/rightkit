import { buildHtmlEditableBody } from "./htmlMutationEngine.js";
import type {
  HtmlMutationPlan,
  HtmlSourcePatch,
  HtmlSourceRange,
  HtmlRevision,
} from "./htmlMutationEngine.js";
import {
  HtmlOperationError,
  allLocations,
  locateElement,
  patch,
  rangeKey,
} from "./htmlPlanLocations.js";
import type { HtmlElementLocation, LocatedNode } from "./htmlPlanLocations.js";
import {
  tableDeleteColumnPlan,
  tableDeleteRowPlan,
  tableInsertColumnPlan,
  tableInsertRowPlan,
} from "./htmlTablePlans.js";

/**
 * Operation-aware source-surgery planners (HTML-EDITING-ARCHITECTURE-FINAL.md §14).
 *
 * Each planner turns an explicit operation + a validated source-backed target into
 * the narrowest atomic patch set: tag-name replacement over reserialisation,
 * attribute-value replacement, zero-width insertions, never whole-fragment
 * regeneration when narrower surgery is safe. The mutation boundary is the
 * maximum permitted blast radius, not the intended one.
 *
 * Planners are pure: they resolve against current canonical source and return a
 * plan (or a typed rejection). `commitHtmlMutation` remains the only committer.
 */

export type HtmlOperationName =
  | "replaceText"
  | "toggleMark"
  | "renameBlock"
  | "setAttribute"
  | "setLink"
  | "removeLink"
  | "wrap"
  | "unwrap"
  | "insertSibling"
  | "listInsertItem"
  | "listDeleteItem"
  | "tableInsertRow"
  | "tableDeleteRow"
  | "tableInsertColumn"
  | "tableDeleteColumn"
  | "imageInsert"
  | "imageReplace"
  | "rawReplace"
  | "deleteNode"
  | "moveSiblingUp"
  | "moveSiblingDown";

/**
 * A block-editor structural/attribute operation the toolbar proposes. Link/image
 * ops carry the reported href/src of the INLINE element the double-click landed
 * on (untrusted locator); the planner re-resolves it against canonical source.
 */
export type RichBlockOp =
  | { kind: "wrap" }
  | { kind: "unwrap" }
  | { kind: "deleteNode" }
  | { kind: "moveUp" }
  | { kind: "moveDown" }
  | { kind: "insertSibling"; tag: string; position: "before" | "after" }
  | { kind: "setLink"; href: string }
  | { kind: "removeLink" }
  | { kind: "imageInsert"; src: string; position: "start" | "end" }
  | { kind: "imageReplace"; src: string }
  | { kind: "setAttribute"; name: string; value: string }
  | { kind: "removeAttribute"; name: string };

const RENAMEABLE_TAGS = new Set([
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
  "figure",
  "figcaption",
  "li",
  "td",
  "th",
]);

// ── Block rename (attribute-preserving tag-name surgery) ─────────────────────

/**
 * Rename `<p ...>…</p>` → `<h2 …>…</h2>` by patching ONLY the two tag-name
 * characters in the open/close tags; attributes, quote style and inner bytes
 * are untouched. Rejects void or list-internal targets that cannot rename safely.
 */
export function renameBlockPlan(
  source: string,
  target: HtmlSourceRange,
  newTag: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const tag = newTag.trim().toLowerCase();
  if (!RENAMEABLE_TAGS.has(tag))
    throw new HtmlOperationError("unsupported", `Cannot rename a block to <${newTag}>.`);
  const el = locateElement(source, target);
  if (!el)
    throw new HtmlOperationError("target-unresolved", "Block target does not resolve in source.");
  if (!el.endTag)
    throw new HtmlOperationError("target-unresolved", "Block has no authored end tag.");
  if (el.tagName === tag)
    throw new HtmlOperationError("incompatible", "Block is already that tag.");
  // Avoid renaming into an incompatible content model inside a list/table cell.
  if (el.tagName === "li" && !["li", "p", "h1", "h2", "h3", "h4", "h5", "h6"].includes(tag))
    throw new HtmlOperationError("incompatible", `Cannot make a list item a <${tag}>.`);

  // `<p …>` — the tag name starts after the `<`; `</p>` — after the `</`. Expected slices are the
  // ACTUAL source bytes (parse5 normalizes the tag name to lowercase, so an authored <P> would
  // otherwise fail the exact-slice guard with a misleading stale error).
  const openRange = {
    start: el.startTag.start + 1,
    end: el.startTag.start + 1 + el.tagName.length,
  };
  const closeRange = { start: el.endTag.start + 2, end: el.endTag.start + 2 + el.tagName.length };
  const openPatch = patch(openRange, source.slice(openRange.start, openRange.end), tag);
  const closePatch = patch(closeRange, source.slice(closeRange.start, closeRange.end), tag);
  return {
    operation: "renameBlock",
    expectedRevision,
    mutationBoundary: { ...target },
    impactBoundary: { ...target },
    patches: [openPatch, closePatch],
  };
}

// ── Attribute surgery ─────────────────────────────────────────────────────────

/**
 * A syntactically valid HTML attribute name. Anything else is refused rather than
 * interpolated: an unvalidated name is a markup-injection hole, because the name is
 * written into the start tag verbatim (`x" onload="alert(1)=` would close the tag's
 * quoting and author an event handler into the user's file).
 */
const ATTR_NAME = /^[A-Za-z_:][\w.:-]*$/;

/** Replace an attribute VALUE (quotes untouched) or insert the attribute into the start tag. */
export function setAttributePlan(
  source: string,
  target: HtmlSourceRange,
  attrName: string,
  newValue: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, target);
  if (!el)
    throw new HtmlOperationError(
      "target-unresolved",
      "Attribute target does not resolve in source.",
    );
  const name = attrName.toLowerCase();
  if (!ATTR_NAME.test(name))
    throw new HtmlOperationError(
      "unsupported",
      `${JSON.stringify(attrName)} is not a valid attribute name.`,
    );
  // Values arrive decoded from controls; escape ampersands before quote entities so literal
  // character-reference text (for example &copy;) survives save/reload unchanged.
  const escapedValue = newValue.replace(/&/g, "&amp;");
  const existing = el.attrValues[name];
  const patches: HtmlSourcePatch[] = [];
  if (existing) {
    // The value range sits INSIDE the quotes, so a raw value containing the closing
    // quote escapes the attribute and authors new ones. Escape against the quote
    // character actually used; for an unquoted value any quote or space breaks out,
    // so rewrite the whole `name=value` span with deterministic double quoting.
    const quote = source[existing.start - 1];
    if (quote === '"') {
      patches.push(
        patch(
          existing,
          source.slice(existing.start, existing.end),
          escapedValue.replace(/"/g, "&quot;"),
        ),
      );
    } else if (quote === "'") {
      patches.push(
        patch(
          existing,
          source.slice(existing.start, existing.end),
          escapedValue.replace(/'/g, "&#39;"),
        ),
      );
    } else {
      const whole = el.attrs.find((a) => a.name === name)?.range;
      if (!whole)
        throw new HtmlOperationError(
          "target-unresolved",
          "Unquoted attribute has no resolvable span.",
        );
      patches.push(
        patch(
          whole,
          source.slice(whole.start, whole.end),
          `${name}="${escapedValue.replace(/"/g, "&quot;")}"`,
        ),
      );
    }
  } else {
    // Insert ` name="value"` right before the start tag's closing `>`.
    const openRaw = source.slice(el.startTag.start, el.startTag.end);
    const closeIndex = openRaw.lastIndexOf(">");
    if (closeIndex < 0)
      throw new HtmlOperationError("target-unresolved", "Start tag has no closing >.");
    const insertAt = el.startTag.start + closeIndex;
    // Deterministic attribute quoting: always double-quoted, `"` escaped as &quot;.
    const escaped = escapedValue.replace(/"/g, "&quot;");
    patches.push(patch({ start: insertAt, end: insertAt }, "", ` ${name}="${escaped}"`));
  }
  return {
    operation: "setAttribute",
    expectedRevision,
    mutationBoundary: { ...target },
    impactBoundary: { ...target },
    patches,
  };
}

/** Remove one authored attribute while preserving every other start-tag byte exactly. */
export function removeAttributePlan(
  source: string,
  target: HtmlSourceRange,
  attrName: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, target);
  if (!el)
    throw new HtmlOperationError(
      "target-unresolved",
      "Attribute target does not resolve in source.",
    );
  const name = attrName.trim().toLowerCase();
  if (!ATTR_NAME.test(name))
    throw new HtmlOperationError(
      "unsupported",
      `${JSON.stringify(attrName)} is not a valid attribute name.`,
    );
  const authored = el.attrs.find((attribute) => attribute.name.toLowerCase() === name);
  if (!authored)
    throw new HtmlOperationError("target-unresolved", `Attribute ${name} no longer exists.`);
  // Include surrounding whitespace when possible so removing the attribute cannot leave a doubled
  // gap in the authored start tag; the untouched quote/style of every sibling remains byte-identical.
  let start = authored.range.start;
  let end = authored.range.end;
  while (start > el.startTag.start && /\s/.test(source[start - 1] ?? "")) start--;
  if (start === authored.range.start) {
    while (end < el.startTag.end && /\s/.test(source[end] ?? "")) end++;
  }
  const range = { start, end };
  return {
    operation: "removeAttribute",
    expectedRevision,
    mutationBoundary: { ...target },
    impactBoundary: { ...target },
    patches: [patch(range, source.slice(range.start, range.end), "")],
  };
}

/** Edit or create the `href` on an anchor (attribute-value surgery). */
export function setLinkPlan(
  source: string,
  anchorRange: HtmlSourceRange,
  href: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  return setAttributePlan(source, anchorRange, "href", href, expectedRevision);
}

/** Unwrap an anchor: remove its start/end tags, preserving the inner bytes. */
export function removeLinkPlan(
  source: string,
  anchorRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, anchorRange);
  if (!el || el.tagName !== "a")
    throw new HtmlOperationError("target-unresolved", "Link target does not resolve in source.");
  if (!el.endTag)
    throw new HtmlOperationError("target-unresolved", "Link has no authored end tag.");
  return {
    operation: "removeLink",
    expectedRevision,
    mutationBoundary: { ...anchorRange },
    impactBoundary: { ...anchorRange },
    patches: [
      patch(el.startTag, source.slice(el.startTag.start, el.startTag.end), ""),
      patch(el.endTag, source.slice(el.endTag.start, el.endTag.end), ""),
    ],
  };
}

/** Replace an `<img>` src value (attribute surgery on a void element). */
export function imageReplacePlan(
  source: string,
  imageRange: HtmlSourceRange,
  src: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, imageRange);
  if (!el || el.tagName !== "img")
    throw new HtmlOperationError("target-unresolved", "Image target does not resolve in source.");
  if (!el.attrValues.src)
    throw new HtmlOperationError("target-unresolved", "Image has no src attribute to replace.");
  return {
    operation: "imageReplace",
    expectedRevision,
    mutationBoundary: { ...imageRange },
    impactBoundary: { ...imageRange },
    patches: [
      patch(el.attrValues.src, source.slice(el.attrValues.src.start, el.attrValues.src.end), src),
    ],
  };
}

// ── Wrap / unwrap ────────────────────────────────────────────────────────────

/**
 * Validate that a wrap range coincides with authored node boundaries: the range
 * must start and end on an authored element boundary and span one or more whole
 * sibling elements sharing a parent. A range that starts inside a tag or splits
 * an element is refused.
 */
function assertWrapRangeIsSiblingBoundary(source: string, range: HtmlSourceRange): void {
  const nodes = allLocations(source).nodes;
  const startNode = nodes.find((n) => n.location.range.start === range.start);
  const endNode = nodes.find((n) => n.location.range.end === range.end);
  if (!startNode || !endNode)
    throw new HtmlOperationError(
      "ambiguous",
      "Wrap range must start and end on authored element boundaries.",
    );
  if (startNode.location.range.start > endNode.location.range.start)
    throw new HtmlOperationError(
      "ambiguous",
      "Wrap range is reversed relative to authored elements.",
    );
  const parentOf = (n: LocatedNode): HtmlElementLocation | null =>
    n.ancestors[n.ancestors.length - 1] ?? null;
  const startParent = parentOf(startNode);
  const endParent = parentOf(endNode);
  const sameParent =
    (startParent === null && endParent === null) ||
    (startParent !== null &&
      endParent !== null &&
      startParent.range.start === endParent.range.start &&
      startParent.range.end === endParent.range.end);
  if (!sameParent)
    throw new HtmlOperationError(
      "ambiguous",
      "Wrap range must span sibling elements sharing a parent.",
    );
}

/** Wrap a range in `<tag>…</tag>` with two zero-width insertions. */
export function wrapPlan(
  source: string,
  range: HtmlSourceRange,
  tag: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const t = tag.trim().toLowerCase();
  if (!RENAMEABLE_TAGS.has(t) && !["ul", "ol", "table"].includes(t))
    throw new HtmlOperationError("unsupported", `Cannot wrap with <${tag}>.`);
  if (range.start >= range.end)
    throw new HtmlOperationError("target-unresolved", "Wrap range is empty.");
  assertWrapRangeIsSiblingBoundary(source, range);
  return {
    operation: "wrap",
    expectedRevision,
    mutationBoundary: { ...range },
    impactBoundary: { ...range },
    patches: [
      patch({ start: range.start, end: range.start }, "", `<${t}>`),
      patch({ start: range.end, end: range.end }, "", `</${t}>`),
    ],
  };
}

/** Unwrap a container element: remove its start/end tags, preserving inner bytes. */
export function unwrapPlan(
  source: string,
  elementRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, elementRange);
  if (!el)
    throw new HtmlOperationError("target-unresolved", "Unwrap target does not resolve in source.");
  if (!el.endTag)
    throw new HtmlOperationError("target-unresolved", "Container has no authored end tag.");
  return {
    operation: "unwrap",
    expectedRevision,
    mutationBoundary: { ...elementRange },
    impactBoundary: { ...elementRange },
    patches: [
      patch(el.startTag, source.slice(el.startTag.start, el.startTag.end), ""),
      patch(el.endTag, source.slice(el.endTag.start, el.endTag.end), ""),
    ],
  };
}

/** Delete one authored element entirely (single-patch removal). */
export function deleteNodePlan(
  source: string,
  elementRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, elementRange);
  if (!el)
    throw new HtmlOperationError("target-unresolved", "Delete target does not resolve in source.");
  // Removing an element merges the whitespace text nodes that surrounded it into one, so the
  // parsed node multiset just outside the element's own span legitimately changes. Declare the
  // impact as the authored parent (clamped to the editable body, since a top-level block's parent
  // is <body> whose authored range spans the shell) so that sibling reshuffle is inside the region
  // the structural check owns — mirroring insertSiblingPlan.
  const parent = parentRangeOf(source, elementRange);
  const editable = buildHtmlEditableBody(source);
  const impact = parent
    ? { start: Math.max(parent.start, editable.start), end: Math.min(parent.end, editable.end) }
    : { ...elementRange };
  return {
    operation: "deleteNode",
    expectedRevision,
    mutationBoundary: { ...elementRange },
    impactBoundary: impact,
    patches: [patch(elementRange, source.slice(elementRange.start, elementRange.end), "")],
  };
}

/** The authored immediate parent of `range`, when one exists in the location cache. */
function parentRangeOf(source: string, range: HtmlSourceRange): HtmlSourceRange | null {
  const entry = allLocations(source).byRange.get(rangeKey(range));
  const parent = entry?.ancestors[entry.ancestors.length - 1];
  return parent ? { start: parent.range.start, end: parent.range.end } : null;
}

/** Direct authored element children of `parentRange`, in source order. */
function siblingElements(source: string, parentRange: HtmlSourceRange): HtmlElementLocation[] {
  return allLocations(source)
    .nodes.filter((n) => {
      const parent = n.ancestors[n.ancestors.length - 1];
      return (
        parent !== undefined &&
        parent.range.start === parentRange.start &&
        parent.range.end === parentRange.end
      );
    })
    .map((n) => n.location);
}

/**
 * Move an authored element one position earlier/later among its element siblings
 * (Gutenberg-without-drag reorder). Each sibling owns a "unit" — its own source
 * range PLUS the inter-block whitespace/indentation immediately preceding it (up
 * to the previous sibling's end, or the parent's opening tag for the first
 * child). Moving one slot swaps the two adjacent units' text wholesale, so each
 * block keeps travelling with its own authored indentation instead of leaving it
 * behind — a hand-formatted three-line body reorders cleanly, not just bare tags
 * jammed together. Two non-overlapping patches, still a swap not a splice.
 * Rejects when no sibling exists in that direction — never widens to reach one.
 * Mirrors `insertSiblingPlan`'s parent-clamped impact.
 */
export function moveSiblingPlan(
  source: string,
  elementRange: HtmlSourceRange,
  direction: "up" | "down",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, elementRange);
  if (!el)
    throw new HtmlOperationError("target-unresolved", "Move target does not resolve in source.");
  const entry = allLocations(source).byRange.get(rangeKey(elementRange));
  const parentLoc = entry?.ancestors[entry.ancestors.length - 1];
  if (!parentLoc)
    throw new HtmlOperationError("target-unresolved", "Move target has no authored parent.");
  const siblings = siblingElements(source, parentLoc.range);
  const index = siblings.findIndex(
    (s) => s.range.start === elementRange.start && s.range.end === elementRange.end,
  );
  if (index === -1)
    throw new HtmlOperationError("target-unresolved", "Move target is not an authored child.");
  const neighbourIndex = direction === "up" ? index - 1 : index + 1;
  if (neighbourIndex < 0 || neighbourIndex >= siblings.length)
    throw new HtmlOperationError(
      "incompatible",
      `No sibling element ${direction === "up" ? "before" : "after"} this one.`,
    );
  const lower = Math.min(index, neighbourIndex);
  const upper = Math.max(index, neighbourIndex);
  const gapStart = (i: number): number =>
    i > 0 ? siblings[i - 1].range.end : parentLoc.startTag.end;
  const unitLower: HtmlSourceRange = { start: gapStart(lower), end: siblings[lower].range.end };
  const unitUpper: HtmlSourceRange = { start: gapStart(upper), end: siblings[upper].range.end };
  const textLower = source.slice(unitLower.start, unitLower.end);
  const textUpper = source.slice(unitUpper.start, unitUpper.end);
  const editable = buildHtmlEditableBody(source);
  const impact = {
    start: Math.max(parentLoc.range.start, editable.start),
    end: Math.min(parentLoc.range.end, editable.end),
  };
  return {
    operation: direction === "up" ? "moveSiblingUp" : "moveSiblingDown",
    expectedRevision,
    mutationBoundary: { ...impact },
    impactBoundary: { ...impact },
    patches: [patch(unitLower, textLower, textUpper), patch(unitUpper, textUpper, textLower)],
  };
}

/** Insert a new sibling element before/after the anchor (zero-width insertion). */
export function insertSiblingPlan(
  source: string,
  anchorRange: HtmlSourceRange,
  tag: string,
  position: "before" | "after",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const anchor = locateElement(source, anchorRange);
  if (!anchor)
    throw new HtmlOperationError("target-unresolved", "Anchor element does not resolve in source.");
  const t = tag.trim().toLowerCase();
  // "table" mirrors wrapPlan's allowlist (structural table ops already exist once one is
  // authored); "hr" is the one void element the block inserter offers (divider) and is
  // self-closing — it has no end tag to pair, unlike every other insertable tag here.
  if (!RENAMEABLE_TAGS.has(t) && !["ul", "ol", "table", "hr"].includes(t))
    throw new HtmlOperationError("unsupported", `Cannot insert a <${tag}> sibling.`);
  const insertion = position === "after" ? anchor.range.end : anchor.range.start;
  const parent = parentRangeOf(source, anchorRange);
  // The immediate parent of a top-level block is <body>, whose authored range includes the
  // shell tags and is therefore wider than the editable body inner. Clamp to the editable body.
  const editable = buildHtmlEditableBody(source);
  const impact = parent
    ? {
        start: Math.max(parent.start, editable.start),
        end: Math.min(parent.end, editable.end),
      }
    : { ...anchorRange };
  return {
    operation: "insertSibling",
    expectedRevision,
    mutationBoundary: { ...anchorRange },
    impactBoundary: impact,
    patches: [
      patch({ start: insertion, end: insertion }, "", t === "hr" ? `<hr>` : `<${t}></${t}>`),
    ],
  };
}

/** Insert an `<img>` at the start or end of an authored element's inner content. */
export function imageInsertPlan(
  source: string,
  anchorRange: HtmlSourceRange,
  src: string,
  position: "start" | "end",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const el = locateElement(source, anchorRange);
  if (!el)
    throw new HtmlOperationError(
      "target-unresolved",
      "Image container does not resolve in source.",
    );
  if (!el.endTag)
    throw new HtmlOperationError("target-unresolved", "Image container has no authored end tag.");
  const escaped = src.replace(/"/g, "&quot;");
  const insertion = position === "start" ? el.startTag.end : el.endTag.start;
  return {
    operation: "imageInsert",
    expectedRevision,
    mutationBoundary: { ...anchorRange },
    impactBoundary: { ...anchorRange },
    patches: [patch({ start: insertion, end: insertion }, "", `<img src="${escaped}">`)],
  };
}

/**
 * Replace one exact source range with raw text (the raw-island / raw-edit path).
 * Raw editing may author malformed HTML, so the CALLER opts in with
 * `allowNewErrorsInsideImpact` (and, where appropriate, relaxed structural
 * validation) at commit time; the planner itself only expresses the exact splice.
 */
export function rawReplacePlan(
  source: string,
  range: HtmlSourceRange,
  replacement: string,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  return {
    operation: "rawReplace",
    expectedRevision,
    mutationBoundary: { ...range },
    impactBoundary: { ...range },
    patches: [patch(range, source.slice(range.start, range.end), replacement)],
  };
}

// ── Lists ────────────────────────────────────────────────────────────────────

function isListTag(tag: string): boolean {
  return tag === "ul" || tag === "ol";
}

/** Insert an empty list item before or after the anchor item. */
export function listInsertItemPlan(
  source: string,
  listRange: HtmlSourceRange,
  anchorItemRange: HtmlSourceRange,
  position: "before" | "after",
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const list = locateElement(source, listRange);
  if (!list || !isListTag(list.tagName))
    throw new HtmlOperationError("target-unresolved", "List target does not resolve in source.");
  const anchor = locateElement(source, anchorItemRange);
  if (!anchor || anchor.tagName !== "li")
    throw new HtmlOperationError("target-unresolved", "Anchor item does not resolve in source.");
  const insertion = position === "after" ? anchor.range.end : anchor.range.start;
  return {
    operation: "listInsertItem",
    expectedRevision,
    mutationBoundary: { ...listRange },
    impactBoundary: { ...listRange },
    patches: [patch({ start: insertion, end: insertion }, "", "<li></li>")],
  };
}

/** Remove one list item (single-patch element removal). */
export function listDeleteItemPlan(
  source: string,
  listRange: HtmlSourceRange,
  anchorItemRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  const list = locateElement(source, listRange);
  if (!list || !isListTag(list.tagName))
    throw new HtmlOperationError("target-unresolved", "List target does not resolve in source.");
  const anchor = locateElement(source, anchorItemRange);
  if (!anchor || anchor.tagName !== "li")
    throw new HtmlOperationError("target-unresolved", "Anchor item does not resolve in source.");
  return {
    operation: "listDeleteItem",
    expectedRevision,
    mutationBoundary: { ...listRange },
    impactBoundary: { ...listRange },
    patches: [patch(anchor.range, source.slice(anchor.range.start, anchor.range.end), "")],
  };
}

// ── Structural contexts: tables and lists (Stage 6 properties surface) ───────

export type StructuralSurface = "table" | "list";
export type StructuralAction =
  | "insertRow"
  | "deleteRow"
  | "insertColumn"
  | "deleteColumn"
  | "insertItem"
  | "deleteItem";

export interface TableCellContext {
  table: HtmlElementLocation;
  row: HtmlElementLocation;
  cell: HtmlElementLocation;
}

export interface ListItemContext {
  list: HtmlElementLocation;
  item: HtmlElementLocation;
}

/** The closest authored ancestor element (strictly) containing `range` and matching `predicate`. */
function containingAncestor(
  source: string,
  range: HtmlSourceRange,
  predicate: (tag: string) => boolean,
): HtmlElementLocation | null {
  let best: HtmlElementLocation | null = null;
  let bestSpan = Infinity;
  for (const { location } of allLocations(source).nodes) {
    const { start, end } = location.range;
    const strictlyContains =
      start <= range.start && end >= range.end && (start < range.start || end > range.end);
    if (!strictlyContains || !predicate(location.tagName)) continue;
    const span = end - start;
    if (span < bestSpan) {
      best = location;
      bestSpan = span;
    }
  }
  return best;
}

/** Resolve the table + row that own a given cell (deepest matching ancestor). */
export function resolveTableCellContext(
  source: string,
  cellRange: HtmlSourceRange,
): TableCellContext | null {
  const cell = locateElement(source, cellRange);
  if (!cell || (cell.tagName !== "td" && cell.tagName !== "th")) return null;
  const row = containingAncestor(source, cellRange, (tag) => tag === "tr");
  const table = containingAncestor(source, cellRange, (tag) => tag === "table");
  if (!row || !table) return null;
  if (!(row.range.start >= table.range.start && row.range.end <= table.range.end)) return null;
  return { table, row, cell };
}

/** Resolve the list that owns a given list item. */
export function resolveListItemContext(
  source: string,
  itemRange: HtmlSourceRange,
): ListItemContext | null {
  const item = locateElement(source, itemRange);
  if (!item || item.tagName !== "li") return null;
  const list = containingAncestor(source, itemRange, (tag) => tag === "ul" || tag === "ol");
  if (!list) return null;
  return { list, item };
}

/**
 * Plan one contextual structural operation for the block under edit: row/column
 * add+delete for table cells, item add+delete for list items. Insertions happen
 * below the anchor row / right of the anchor column / below the anchor item;
 * every operation is one atomic transaction with the surrounding structure as
 * its boundary.
 */
export function planStructuralOp(
  source: string,
  surface: StructuralSurface,
  action: StructuralAction,
  anchorRange: HtmlSourceRange,
  expectedRevision: HtmlRevision,
): HtmlMutationPlan {
  if (surface === "table") {
    const ctx = resolveTableCellContext(source, anchorRange);
    if (!ctx)
      throw new HtmlOperationError("target-unresolved", "No table context resolves the cell.");
    switch (action) {
      case "insertRow":
        return tableInsertRowPlan(
          source,
          ctx.table.range,
          ctx.row.range,
          "after",
          expectedRevision,
        );
      case "deleteRow":
        return tableDeleteRowPlan(source, ctx.table.range, ctx.row.range, expectedRevision);
      case "insertColumn":
        return tableInsertColumnPlan(
          source,
          ctx.table.range,
          ctx.cell.range,
          "after",
          expectedRevision,
        );
      case "deleteColumn":
        return tableDeleteColumnPlan(source, ctx.table.range, ctx.cell.range, expectedRevision);
      default:
        throw new HtmlOperationError(
          "incompatible",
          `Action ${action} does not apply to a table surface.`,
        );
    }
  }
  const ctx = resolveListItemContext(source, anchorRange);
  if (!ctx) throw new HtmlOperationError("target-unresolved", "No list context resolves the item.");
  switch (action) {
    case "insertItem":
      return listInsertItemPlan(source, ctx.list.range, ctx.item.range, "after", expectedRevision);
    case "deleteItem":
      return listDeleteItemPlan(source, ctx.list.range, ctx.item.range, expectedRevision);
    default:
      throw new HtmlOperationError(
        "incompatible",
        `Action ${action} does not apply to a list surface.`,
      );
  }
}

// ── Facade re-exports ────────────────────────────────────────────────────────
// The location kernel, inline-text, and table planners moved to dedicated
// modules; existing consumers keep importing them from this file.
export {
  HtmlOperationError,
  descendantElements,
  locateElement,
  resolveInlineAnchor,
} from "./htmlPlanLocations.js";
export type { HtmlElementLocation } from "./htmlPlanLocations.js";
export { planInlineTextReplace } from "./htmlInlineTextPlan.js";
export {
  tableDeleteColumnPlan,
  tableDeleteRowPlan,
  tableInsertColumnPlan,
  tableInsertRowPlan,
} from "./htmlTablePlans.js";
