import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import type { MarkdownCommitSummary } from "../core/MarkdownDocumentTypes.js";
import type { MarkdownTextEdit } from "../core/types.js";

export type MarkdownLinkDialogMode = "insert" | "edit" | "protected";

export interface MarkdownLinkRange {
  readonly from: number;
  readonly to: number;
  readonly syntax?: string;
}

export interface MarkdownProtectedRange {
  readonly from: number;
  readonly to: number;
  readonly reason: string;
}

export interface MarkdownLinkSelectionInput {
  readonly anchor: number;
  readonly head: number;
  readonly existingLink?: MarkdownLinkRange;
  readonly protectedRange?: MarkdownProtectedRange;
}

export interface MarkdownLinkDraft {
  readonly text: string;
  readonly address: string;
  readonly title: string;
}

export interface MarkdownLinkModel {
  readonly mode: MarkdownLinkDialogMode;
  readonly from: number;
  readonly to: number;
  readonly anchor: number;
  readonly head: number;
  /** Raw selected source text, used as the default label for insertion. */
  readonly selectedText: string;
  readonly text: string;
  readonly address: string;
  readonly title: string;
  readonly protectedReason: string | null;
  readonly existing: ParsedMarkdownLink | null;
}

export interface ParsedMarkdownLink {
  readonly label: string;
  readonly destination: string;
  readonly title: string | null;
}

export interface MarkdownLinkEditResult {
  readonly ok: true;
  readonly edit: MarkdownTextEdit;
  readonly selection: { readonly anchor: number; readonly head: number };
}

export interface MarkdownLinkErrorResult {
  readonly ok: false;
  readonly error: string;
}

export type MarkdownLinkBuildResult = MarkdownLinkEditResult | MarkdownLinkErrorResult;

export interface MarkdownLinkCommitResult extends MarkdownLinkEditResult {
  readonly summary: MarkdownCommitSummary;
}

/** Return a concise user-facing validation result for an address field. */
export function validateLinkDestination(
  destination: string,
):
  | { readonly valid: true; readonly value: string }
  | { readonly valid: false; readonly error: string } {
  const value = destination.trim();
  if (!value) return { valid: false, error: "Enter a link address." };
  // biome-ignore lint/suspicious/noControlCharactersInRegex: intentional URL safety check
  if (/[\u0000-\u001F\u007F\u2028\u2029]/.test(value)) {
    return { valid: false, error: "Link address contains an invalid control character." };
  }
  if (/[<>"'`]/.test(value))
    return { valid: false, error: "Link address contains invalid characters." };
  if (isUnsafeLinkDestination(value)) {
    return {
      valid: false,
      error: "javascript:, data:, and other active link schemes are not allowed.",
    };
  }
  const scheme = value.match(/^([a-z][a-z\d+.-]*):/i)?.[1]?.toLowerCase();
  if (scheme && !["http", "https", "mailto", "tel"].includes(scheme)) {
    return { valid: false, error: `The ${scheme}: link scheme is not allowed.` };
  }
  if (value.startsWith("//")) {
    return {
      valid: false,
      error: "Protocol-relative addresses are not allowed; use https:// instead.",
    };
  }
  return { valid: true, value };
}

/** Allow only URL schemes supported by rendered Markdown links. */
export function isAllowedLinkDestination(url: string): boolean {
  return validateLinkDestination(url).valid;
}

/** Used when the UI needs to distinguish an active scheme from other invalid input. */
export function isUnsafeLinkDestination(url: string): boolean {
  const trimmed = url.trim().toLowerCase();
  return /^(?:javascript|data|vbscript|file):/.test(trimmed);
}

export function escapeLinkLabel(label: string): string {
  return label.replace(/\\/g, "\\\\").replace(/\[/g, "\\[").replace(/\]/g, "\\]");
}

/** Escape a bare Markdown destination while keeping its URL meaning intact. */
export function escapeLinkDestination(destination: string): string {
  return destination
    .trim()
    .replace(/\\/g, "\\\\")
    .replace(/\(/g, "\\(")
    .replace(/\)/g, "\\)")
    .replace(/ /g, "%20");
}

export function escapeLinkTitle(title: string): string {
  return title
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/[\r\n]+/g, " ");
}

export function buildMarkdownInlineLink(
  label: string,
  destination: string,
  title?: string | null,
): string {
  const validation = validateLinkDestination(destination);
  if (!validation.valid) throw new Error(validation.error);
  const titlePart = title?.trim() ? ` "${escapeLinkTitle(title)}"` : "";
  return `[${escapeLinkLabel(label)}](${escapeLinkDestination(validation.value)}${titlePart})`;
}

/**
 * Parse the inline link shape emitted by {@link buildMarkdownInlineLink}, plus
 * ordinary Markdown links with a quoted, apostrophe-quoted, or parenthesized title.
 */
export function parseLinkSyntax(markdown: string): ParsedMarkdownLink | null {
  const closeBracket = findClosingBracket(markdown);
  if (closeBracket < 0 || markdown[closeBracket + 1] !== "(") return null;
  const closeParen = findClosingParen(markdown, closeBracket + 1);
  if (closeParen !== markdown.length - 1) return null;
  const rawLabel = markdown.slice(1, closeBracket);
  const parsedDestination = parseDestinationAndTitle(markdown.slice(closeBracket + 2, closeParen));
  if (!parsedDestination) return null;
  return {
    label: unescapeMarkdownPunctuation(rawLabel),
    destination: decodeDestination(parsedDestination.destination),
    title:
      parsedDestination.title === null
        ? null
        : unescapeMarkdownPunctuation(parsedDestination.title),
  };
}

/** Resolve the URL-first dialog state from canonical source coordinates. */
export function resolveMarkdownLinkModel(input: {
  readonly source: string;
  readonly selection: MarkdownLinkSelectionInput;
  readonly links?:
    | readonly MarkdownLinkRange[]
    | readonly {
        readonly kind: string;
        readonly range: { readonly from: number; readonly to: number };
      }[];
  readonly protectedRanges?: readonly MarkdownProtectedRange[];
}): MarkdownLinkModel {
  const sourceLength = input.source.length;
  const anchor = clampOffset(input.selection.anchor, sourceLength);
  const head = clampOffset(input.selection.head, sourceLength);
  const from = Math.min(anchor, head);
  const to = Math.max(anchor, head);
  const selectedText = input.source.slice(from, to);
  const protectedRange =
    input.selection.protectedRange ??
    input.protectedRanges?.find((range) => containsSelection(range, from, to));
  if (protectedRange) {
    return {
      mode: "protected",
      from: protectedRange.from,
      to: protectedRange.to,
      anchor,
      head,
      selectedText,
      text: "",
      address: "",
      title: "",
      protectedReason: protectedRange.reason,
      existing: null,
    };
  }

  const linkRange =
    input.selection.existingLink ?? findLinkRange(input.source, from, to, input.links);
  if (linkRange) {
    const syntax = linkRange.syntax ?? input.source.slice(linkRange.from, linkRange.to);
    const existing = parseLinkSyntax(syntax);
    if (existing) {
      return {
        mode: "edit",
        from: linkRange.from,
        to: linkRange.to,
        anchor,
        head,
        selectedText,
        text: existing.label,
        address: existing.destination,
        title: existing.title ?? "",
        protectedReason: null,
        existing,
      };
    }
  }

  return {
    mode: "insert",
    from,
    to,
    anchor,
    head,
    selectedText,
    text: selectedText,
    address: "",
    title: "",
    protectedReason: null,
    existing: null,
  };
}

/** Build one source edit for Apply or Remove. */
export function buildMarkdownLinkEdit(
  model: MarkdownLinkModel,
  draft: MarkdownLinkDraft,
  remove = false,
): MarkdownLinkBuildResult {
  if (model.mode === "protected") {
    return {
      ok: false,
      error: model.protectedReason ?? "This content can only be edited in Source.",
    };
  }
  if (remove) {
    if (!model.existing) return { ok: false, error: "No existing link is selected." };
    const label = model.existing.label;
    const start = model.anchor === model.head ? model.from : model.from;
    const end = start + label.length;
    return {
      ok: true,
      edit: { from: model.from, to: model.to, insert: label, inheritMode: "preserve" },
      selection:
        model.anchor < model.head
          ? { anchor: start, head: end }
          : model.anchor > model.head
            ? { anchor: end, head: start }
            : { anchor: start + label.length, head: start + label.length },
    };
  }

  const address = validateLinkDestination(draft.address);
  if (!address.valid) return { ok: false, error: address.error };
  const label = draft.text || model.selectedText || address.value;
  if (!label) return { ok: false, error: "Enter link text or select text first." };
  const insert = buildMarkdownInlineLink(label, address.value, draft.title || null);
  return {
    ok: true,
    edit: { from: model.from, to: model.to, insert, inheritMode: "preserve" },
    selection: selectionAfterLinkEdit(
      model,
      model.from,
      model.anchor === model.head ? insert.length : escapeLinkLabel(label).length,
      model.anchor === model.head ? insert.length : escapeLinkLabel(label).length,
    ),
  };
}

/** Apply a link edit through core as one isolated history command. */
export function applyMarkdownLinkCommand(
  core: MarkdownDocumentCore,
  model: MarkdownLinkModel,
  draft: MarkdownLinkDraft,
  remove = false,
): MarkdownLinkCommitResult | MarkdownLinkErrorResult {
  const result = buildMarkdownLinkEdit(model, draft, remove);
  if (!result.ok) return result;
  core.postSelectionHint({
    surface: "prosemirror",
    nodeId: null,
    anchor: result.selection.anchor,
    head: result.selection.head,
    assoc: 1,
  });
  const summary = core.applyCommand({
    edits: [result.edit],
    expectedRevision: core.snapshot().revision,
    origin: "block-editor",
    label: "Format",
    isolateHistory: true,
  });
  return { ...result, summary };
}

/** Compute a relative Markdown path when both paths share a local root. */
export function relativeMarkdownLinkPath(
  documentPath: string | null | undefined,
  targetPath: string,
  fallbackRelativePath?: string,
): string | null {
  if (!documentPath) return null;
  const document = splitPath(documentPath);
  const target = splitPath(targetPath);
  if (!document || !target || document.root.toLowerCase() !== target.root.toLowerCase()) {
    return fallbackRelativePath ?? null;
  }
  const parent = document.parts.slice(0, -1);
  let common = 0;
  while (common < parent.length && common < target.parts.length) {
    const parentPart = parent[common];
    const targetPart = target.parts[common];
    if (!parentPart || !targetPart || parentPart.toLowerCase() !== targetPart.toLowerCase()) break;
    common++;
  }
  const relative = [
    ...Array.from({ length: parent.length - common }, () => ".."),
    ...target.parts.slice(common),
  ];
  return (
    relative.join("/") || target.parts[target.parts.length - 1] || fallbackRelativePath || null
  );
}

export function headingFragment(headingId: string | undefined): string | null {
  return headingId ? `#${headingId}` : null;
}

function findLinkRange(
  source: string,
  from: number,
  to: number,
  links?:
    | readonly MarkdownLinkRange[]
    | readonly { kind: string; range: { from: number; to: number } }[],
): MarkdownLinkRange | null {
  const candidates = (links ?? []).filter((candidate) => {
    if ("kind" in candidate && candidate.kind !== "link") return false;
    const range = "range" in candidate ? candidate.range : candidate;
    return range.from <= from && range.to >= to && range.to > range.from;
  });
  candidates.sort((left, right) => {
    const a = "range" in left ? left.range : left;
    const b = "range" in right ? right.range : right;
    return a.to - a.from - (b.to - b.from);
  });
  const first = candidates[0];
  if (first) {
    const range = "range" in first ? first.range : first;
    return { from: range.from, to: range.to, syntax: source.slice(range.from, range.to) };
  }
  return null;
}

function selectionAfterLinkEdit(
  model: MarkdownLinkModel,
  from: number,
  labelLength: number,
  replacementLength: number,
): { anchor: number; head: number } {
  if (model.anchor === model.head) {
    const position = model.mode === "edit" ? from + replacementLength : from + replacementLength;
    return { anchor: position, head: position };
  }
  const start = model.mode === "edit" ? from + 1 : from + 1;
  const end = start + labelLength;
  return model.anchor < model.head ? { anchor: start, head: end } : { anchor: end, head: start };
}

function containsSelection(range: MarkdownProtectedRange, from: number, to: number): boolean {
  return from === to ? from >= range.from && from <= range.to : from < range.to && to > range.from;
}

function findClosingBracket(markdown: string): number {
  if (!markdown.startsWith("[")) return -1;
  let depth = 0;
  for (let i = 0; i < markdown.length; i++) {
    const char = markdown[i];
    if (char === "\\") {
      i++;
      continue;
    }
    if (char === "[") depth++;
    if (char === "]" && --depth === 0) return i;
  }
  return -1;
}

function findClosingParen(markdown: string, opening: number): number {
  let depth = 0;
  for (let i = opening; i < markdown.length; i++) {
    const char = markdown[i];
    if (char === "\\") {
      i++;
      continue;
    }
    if (char === "(") depth++;
    if (char === ")" && --depth === 0) return i;
  }
  return -1;
}

function parseDestinationAndTitle(
  inside: string,
): { destination: string; title: string | null } | null {
  const trimmed = inside.trim();
  if (!trimmed) return null;
  let destination = trimmed;
  let rest = "";
  if (trimmed.startsWith("<")) {
    const close = findUnescaped(trimmed, ">", 1);
    if (close < 0) return null;
    destination = trimmed.slice(1, close);
    rest = trimmed.slice(close + 1).trim();
  } else {
    for (let i = 0; i < trimmed.length; i++) {
      if (trimmed[i] === "\\") {
        i++;
        continue;
      }
      if (/\s/.test(trimmed[i] ?? "") && /^["'(]/.test(trimmed.slice(i).trimStart())) {
        destination = trimmed.slice(0, i);
        rest = trimmed.slice(i).trim();
        break;
      }
    }
  }
  if (!rest) return { destination, title: null };
  const delimiter = rest[0];
  const closing = delimiter === "(" ? ")" : delimiter;
  if (!delimiter || !closing || rest[rest.length - 1] !== closing) return null;
  return { destination, title: rest.slice(1, -1) };
}

function findUnescaped(value: string, expected: string, start: number): number {
  for (let i = start; i < value.length; i++) {
    if (value[i] === "\\") {
      i++;
      continue;
    }
    if (value[i] === expected) return i;
  }
  return -1;
}

function unescapeMarkdownPunctuation(value: string): string {
  return value.replace(/\\([\\[\]()<>'"`])/g, "$1");
}

function decodeDestination(value: string): string {
  return unescapeMarkdownPunctuation(value).replace(/%20/gi, " ");
}

function clampOffset(offset: number, length: number): number {
  return Math.max(0, Math.min(length, Math.trunc(offset)));
}

function splitPath(value: string): { root: string; parts: string[] } | null {
  const normalized = value.replace(/\\/g, "/").replace(/\/+/g, "/");
  const match = normalized.match(/^([A-Za-z]:|\/|)(.*)$/);
  if (!match) return null;
  const root = match[1] ?? "";
  const parts = (match[2] ?? "").split("/").filter(Boolean);
  return { root, parts };
}
