import type {
  HtmlMutationPlan,
  HtmlRevision,
  HtmlSourcePatch,
  HtmlSourceRange,
} from "./htmlMutationEngine.js";
import {
  HtmlOperationError,
  locateElement,
  resolveInlineAnchor,
} from "./htmlPlanLocations.js";
import type { HtmlElementLocation } from "./htmlPlanLocations.js";
import { planInlineTextReplace } from "./htmlInlineTextPlan.js";
import {
  deleteNodePlan,
  imageInsertPlan,
  imageReplacePlan,
  insertSiblingPlan,
  moveSiblingPlan,
  planStructuralOp,
  rawReplacePlan,
  removeAttributePlan,
  removeLinkPlan,
  renameBlockPlan,
  setAttributePlan,
  setLinkPlan,
  unwrapPlan,
  wrapPlan,
} from "./htmlOperationPlanner.js";
import type { RichBlockOp, StructuralAction, StructuralSurface } from "./htmlOperationPlanner.js";

/**
 * Operation-aware plan resolver (HTML-EDITING-ARCHITECTURE-FINAL.md section 14).
 *
 * `resolveMutationPlan(source, request, revision)` replaces the assumption that the clicked
 * element is the write range: each operation class owns its target eligibility, smallest safe
 * mutation boundary, structural-impact boundary and patch strategy, and returns either a plan
 * plus a human-readable scope (section 15) or a typed rejection. It never commits;
 * `commitHtmlMutation` stays the single writer.
 */

export type HtmlMutationRequest =
  | {
      kind: "replaceText";
      /** Outer source range of the authored block element being edited. */
      target: HtmlSourceRange;
      /** New inner HTML of that block (the editor's serialized content). */
      newInnerHtml: string;
      /** Optional block rename (`p` -> `h2`), applied with the text edit as one transaction. */
      targetTag?: string;
      /** Marks the edit as a bold/italic/etc. toggle for labelling only. */
      mark?: boolean;
    }
  | {
      kind: "blockOp";
      target: HtmlSourceRange;
      op: RichBlockOp;
      /** Reported identity of the inline link/image the gesture landed on (untrusted locator). */
      inline?: { href?: string; src?: string };
    }
  | {
      kind: "structural";
      surface: StructuralSurface;
      action: StructuralAction;
      anchor: HtmlSourceRange;
    }
  | { kind: "rawReplace"; range: HtmlSourceRange; text: string };

export type HtmlPlanStrategy = "narrow-patch" | "inner-regenerate" | "element-surgery" | "raw";

export interface HtmlPlanScope {
  /** Semantic owner of the operation, e.g. "Paragraph", "Table". */
  owner: string;
  operation: string;
  /** True when the impact boundary is wider than the clicked/target element. */
  widened: boolean;
  /** True when a surprising widening must be surfaced before commit (section 15). */
  confirmBeforeCommit: boolean;
  description: string;
}

export type HtmlMutationResolution =
  | {
      ok: true;
      plan: HtmlMutationPlan;
      target: HtmlElementLocation | null;
      scope: HtmlPlanScope;
      editContext: { strategy: HtmlPlanStrategy; patchCount: number };
    }
  | { ok: false; code: HtmlOperationError["code"] | "invalid-request"; reason: string };

const OWNER_LABELS: Record<string, string> = {
  p: "Paragraph",
  li: "List item",
  ul: "List",
  ol: "List",
  table: "Table",
  tr: "Table row",
  td: "Table cell",
  th: "Table cell",
  img: "Image",
  a: "Link",
  blockquote: "Quote",
  pre: "Code block",
  div: "Container",
};

function ownerLabel(tag: string | undefined): string {
  if (!tag) return "Selection";
  if (/^h[1-6]$/.test(tag)) return "Heading";
  return OWNER_LABELS[tag] ?? `<${tag}> element`;
}

function sameRange(a: HtmlSourceRange, b: HtmlSourceRange): boolean {
  return a.start === b.start && a.end === b.end;
}

function reject(error: unknown): HtmlMutationResolution {
  if (error instanceof HtmlOperationError)
    return { ok: false, code: error.code, reason: error.message };
  return { ok: false, code: "invalid-request", reason: error instanceof Error ? error.message : String(error) };
}

/** Operations whose natural owner is wider than the clicked block and that proceed directly. */
const NATURAL_WIDE = /^(table|list)/;
/** Operations that rewrite surrounding structure: surface the scope before commit. */
const CONFIRM_WIDE = new Set(["wrap", "unwrap", "deleteNode", "rawReplace"]);

function describeScope(
  plan: HtmlMutationPlan,
  target: HtmlElementLocation | null,
  ownerTag: string | undefined,
): HtmlPlanScope {
  const widened = target ? !sameRange(plan.impactBoundary, target.range) : false;
  const natural = NATURAL_WIDE.test(plan.operation);
  return {
    owner: ownerLabel(ownerTag),
    operation: plan.operation,
    widened,
    confirmBeforeCommit: widened && !natural && CONFIRM_WIDE.has(plan.operation),
    description: `Editing: ${ownerLabel(ownerTag)}; operation: ${plan.operation}; scope: ${
      widened ? "enclosing structure" : "this element"
    }`,
  };
}

function innerRangeOf(el: HtmlElementLocation): HtmlSourceRange | null {
  return el.endTag ? { start: el.startTag.end, end: el.endTag.start } : null;
}

function resolveReplaceText(
  source: string,
  req: Extract<HtmlMutationRequest, { kind: "replaceText" }>,
  revision: HtmlRevision,
): HtmlMutationResolution {
  const el = locateElement(source, req.target);
  if (!el) throw new HtmlOperationError("target-unresolved", "Block target does not resolve in source.");
  const inner = innerRangeOf(el);
  if (!inner)
    throw new HtmlOperationError("target-unresolved", "Block has no authored end tag to edit inside.");

  // Untouched authored wrappers/siblings keep their exact bytes through source-location
  // correspondence; `null` means the shape is structural, so regenerate the inner range only.
  const narrow = planInlineTextReplace(source, inner, req.newInnerHtml);
  const innerPatches: HtmlSourcePatch[] =
    narrow ??
    (source.slice(inner.start, inner.end) === req.newInnerHtml
      ? []
      : [
          {
            range: { ...inner },
            expectedText: source.slice(inner.start, inner.end),
            text: req.newInnerHtml,
          },
        ]);

  const tag = req.targetTag?.replace(/^html:/, "").trim().toLowerCase();
  const rename = tag && tag !== el.tagName ? renameBlockPlan(source, req.target, tag, revision) : null;
  const patches: HtmlSourcePatch[] = [...(rename?.patches ?? []), ...innerPatches];
  const plan: HtmlMutationPlan = {
    operation: rename ? "renameBlock" : req.mark ? "toggleMark" : "replaceText",
    expectedRevision: revision,
    // A rename owns the whole element; a plain text edit owns only the inner range.
    mutationBoundary: { ...(rename ? el.range : inner) },
    impactBoundary: { ...el.range },
    patches:
      patches.length > 0
        ? patches
        : // Nothing changed: a zero-width no-op keeps the plan valid without touching bytes.
          [{ range: { start: inner.start, end: inner.start }, expectedText: "", text: "" }],
  };
  return {
    ok: true,
    plan,
    target: el,
    scope: describeScope(plan, el, el.tagName),
    editContext: { strategy: narrow ? "narrow-patch" : "inner-regenerate", patchCount: patches.length },
  };
}

function resolveBlockOp(
  source: string,
  req: Extract<HtmlMutationRequest, { kind: "blockOp" }>,
  revision: HtmlRevision,
): HtmlMutationResolution {
  const { target, op } = req;
  const block = locateElement(source, target);
  if (!block) throw new HtmlOperationError("target-unresolved", "Block target does not resolve in source.");
  const inlineEl = (kind: "link" | "image"): HtmlElementLocation => {
    const found = resolveInlineAnchor(source, target, req.inline ?? {});
    if (!found || found.kind !== kind)
      throw new HtmlOperationError(
        "target-unresolved",
        `The ${kind} no longer resolves uniquely in source.`,
      );
    return found.element;
  };
  let plan: HtmlMutationPlan;
  let ownerTag: string | undefined = block.tagName;
  switch (op.kind) {
    case "wrap":
      plan = wrapPlan(source, target, "div", revision);
      break;
    case "unwrap":
      plan = unwrapPlan(source, target, revision);
      break;
    case "deleteNode":
      plan = deleteNodePlan(source, target, revision);
      break;
    case "moveUp":
      plan = moveSiblingPlan(source, target, "up", revision);
      break;
    case "moveDown":
      plan = moveSiblingPlan(source, target, "down", revision);
      break;
    case "insertSibling":
      plan = insertSiblingPlan(source, target, op.tag, op.position, revision);
      break;
    case "setAttribute":
    case "removeAttribute": {
      // An image click opens its containing block; Properties belongs to the authored <img>.
      const el = req.inline?.src !== undefined ? inlineEl("image") : block;
      ownerTag = el.tagName;
      plan =
        op.kind === "setAttribute"
          ? setAttributePlan(source, el.range, op.name, op.value, revision)
          : removeAttributePlan(source, el.range, op.name, revision);
      break;
    }
    case "setLink": {
      const el = inlineEl("link");
      ownerTag = "a";
      plan = setLinkPlan(source, el.range, op.href, revision);
      break;
    }
    case "removeLink": {
      const el = inlineEl("link");
      ownerTag = "a";
      plan = removeLinkPlan(source, el.range, revision);
      break;
    }
    case "imageReplace": {
      const el = inlineEl("image");
      ownerTag = "img";
      plan = imageReplacePlan(source, el.range, op.src, revision);
      break;
    }
    case "imageInsert":
      plan = imageInsertPlan(source, target, op.src, op.position, revision);
      break;
  }
  return {
    ok: true,
    plan,
    target: block,
    scope: describeScope(plan, block, ownerTag),
    editContext: { strategy: "element-surgery", patchCount: plan.patches.length },
  };
}

export function resolveMutationPlan(
  source: string,
  request: HtmlMutationRequest,
  expectedRevision: HtmlRevision,
): HtmlMutationResolution {
  try {
    switch (request.kind) {
      case "replaceText":
        return resolveReplaceText(source, request, expectedRevision);
      case "blockOp":
        return resolveBlockOp(source, request, expectedRevision);
      case "structural": {
        const plan = planStructuralOp(
          source,
          request.surface,
          request.action,
          request.anchor,
          expectedRevision,
        );
        return {
          ok: true,
          plan,
          target: locateElement(source, request.anchor),
          scope: {
            owner: request.surface === "table" ? "Table" : "List",
            operation: plan.operation,
            widened: true,
            // The command naturally owns its table/list: proceeds without confirmation.
            confirmBeforeCommit: false,
            description: `Editing: ${request.surface === "table" ? "Table" : "List"}; operation: ${plan.operation}; scope: entire ${request.surface} structure`,
          },
          editContext: { strategy: "element-surgery", patchCount: plan.patches.length },
        };
      }
      case "rawReplace": {
        const plan = rawReplacePlan(source, request.range, request.text, expectedRevision);
        return {
          ok: true,
          plan,
          target: null,
          scope: {
            owner: "Source",
            operation: plan.operation,
            widened: false,
            confirmBeforeCommit: false,
            description: "Editing: raw source range; operation: rawReplace",
          },
          editContext: { strategy: "raw", patchCount: 1 },
        };
      }
    }
  } catch (error) {
    return reject(error);
  }
}
