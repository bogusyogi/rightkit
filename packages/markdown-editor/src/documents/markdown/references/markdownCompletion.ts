/**
 * Completion provider for the Markdown source editor.
 *
 * The provider matches the cursor's prefix against a list of
 * completions and returns the ranked options. The source editor
 * calls {@link completionsAt} on every keystroke.
 *
 * Completions are pure-TS: the provider is stateless and reads
 * from the supplied index entries and headings. The provider
 * never blocks; large completions are clipped to the configured
 * limit.
 */

import type { MarkdownIndexedFile } from "./folderIndex";
import type { MarkdownSourceNode } from "../core/MarkdownSourceMap";

export interface CompletionOption {
  readonly label: string;
  readonly detail: string;
  readonly insertText: string;
  readonly kind: "link" | "anchor" | "image" | "footnote";
}

export interface CompletionRequest {
  readonly textBefore: string;
  readonly max?: number;
  readonly folderFiles: readonly MarkdownIndexedFile[];
  readonly sessionFiles: readonly MarkdownIndexedFile[];
  readonly headings: readonly MarkdownSourceNode[];
}

const DEFAULT_MAX = 20;

/**
 * Compute completions at the supplied cursor. Returns an empty
 * array when no completion kind matches.
 */
export function completionsAt(request: CompletionRequest): readonly CompletionOption[] {
  const max = request.max ?? DEFAULT_MAX;
  const trigger = detectTrigger(request.textBefore);
  if (!trigger) return [];
  if (trigger.kind === "link") {
    return completeLink(trigger, request, max);
  }
  if (trigger.kind === "anchor") {
    return completeAnchor(trigger, request, max);
  }
  if (trigger.kind === "image") {
    return completeImage(trigger, request, max);
  }
  return completeFootnote(trigger, request, max);
}

/** Start offset for the active completion payload, or `null` outside Markdown link syntax. */
export function completionStartAt(textBefore: string): number | null {
  return detectTrigger(textBefore)?.from ?? null;
}

interface LinkTrigger {
  readonly kind: "link" | "anchor" | "image" | "footnote";
  readonly prefix: string;
  /** Character offset in the original text where the prefix starts. */
  readonly from: number;
}

function detectTrigger(textBefore: string): LinkTrigger | null {
  // Image trigger must be checked before the link trigger so that
  // `![](...)` doesn't fall into the link branch.
  const imageMatch = /!\[([^\]]*)\]\(([^)]*)$/;
  if (imageMatch.test(textBefore)) {
    const m = imageMatch.exec(textBefore) as RegExpExecArray;
    return { kind: "image", prefix: m[2] ?? "", from: m.index + m[0].length - (m[2] ?? "").length };
  }
  const linkMatch = /\[([^\]]*?)\]\(([^)]*)$/;
  if (linkMatch.test(textBefore)) {
    const m = linkMatch.exec(textBefore) as RegExpExecArray;
    const prefix = m[2] ?? "";
    // Inside a link whose URL starts with `#`, the user is
    // completing an anchor; route to the anchor branch.
    if (prefix.startsWith("#")) {
      return {
        kind: "anchor",
        prefix: prefix.slice(1),
        from: m.index + m[0].length - prefix.length + 1,
      };
    }
    return { kind: "link", prefix, from: m.index + m[0].length - prefix.length };
  }
  const footnoteMatch = /\[\^([^\]]*)$/;
  if (footnoteMatch.test(textBefore)) {
    const m = footnoteMatch.exec(textBefore) as RegExpExecArray;
    return {
      kind: "footnote",
      prefix: m[1] ?? "",
      from: m.index + m[0].length - (m[1] ?? "").length,
    };
  }
  return null;
}

function completeLink(
  trigger: LinkTrigger,
  request: CompletionRequest,
  max: number,
): readonly CompletionOption[] {
  const needle = trigger.prefix.toLowerCase();
  const out: CompletionOption[] = [];
  for (const file of request.sessionFiles) {
    if (!matchesQuery(file, needle)) continue;
    out.push({
      label: file.relativePath,
      detail: "(recent)",
      insertText: file.relativePath,
      kind: "link",
    });
    if (out.length >= max) break;
  }
  for (const file of request.folderFiles) {
    if (out.length >= max) break;
    if (!matchesQuery(file, needle)) continue;
    out.push({
      label: file.relativePath,
      detail: "(folder)",
      insertText: file.relativePath,
      kind: "link",
    });
  }
  return out;
}

function completeAnchor(
  trigger: LinkTrigger,
  request: CompletionRequest,
  max: number,
): readonly CompletionOption[] {
  const needle = trigger.prefix.toLowerCase();
  const out: CompletionOption[] = [];
  for (const heading of request.headings) {
    if (out.length >= max) break;
    const slug = heading.headingId ?? "";
    if (needle.length > 0 && !slug.toLowerCase().includes(needle)) continue;
    out.push({
      label: `#${slug}`,
      detail: heading.semanticKey,
      insertText: slug,
      kind: "anchor",
    });
  }
  return out;
}

function completeImage(
  trigger: LinkTrigger,
  request: CompletionRequest,
  max: number,
): readonly CompletionOption[] {
  const needle = trigger.prefix.toLowerCase();
  const out: CompletionOption[] = [];
  for (const file of [...request.folderFiles, ...request.sessionFiles]) {
    if (out.length >= max) break;
    const ext = file.relativePath.split(".").pop() ?? "";
    if (!["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(ext)) continue;
    if (!matchesQuery(file, needle)) continue;
    out.push({
      label: file.relativePath,
      detail: `(image · ${ext})`,
      insertText: file.relativePath,
      kind: "image",
    });
  }
  return out;
}

function completeFootnote(
  trigger: LinkTrigger,
  request: CompletionRequest,
  max: number,
): readonly CompletionOption[] {
  const needle = trigger.prefix.toLowerCase();
  const out: CompletionOption[] = [];
  for (const heading of request.headings) {
    if (out.length >= max) break;
    const text = heading.semanticKey.toLowerCase();
    if (needle.length > 0 && !text.includes(needle)) continue;
    out.push({
      label: `[^${heading.headingId ?? text}]`,
      detail: heading.semanticKey,
      insertText: heading.headingId ?? text,
      kind: "footnote",
    });
  }
  return out;
}

function matchesQuery(file: MarkdownIndexedFile, needle: string): boolean {
  if (needle.length === 0) return true;
  return (
    file.relativePath.toLowerCase().includes(needle) ||
    (file.title ?? "").toLowerCase().includes(needle)
  );
}
