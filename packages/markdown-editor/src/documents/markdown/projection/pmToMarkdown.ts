import type { Mark, Node as ProseMirrorNode } from "prosemirror-model";
import { serializeGfmTable } from "../tables/markdownTableSerializer";
import { parseCodeFenceSource, serializeCodeFence } from "../codeFenceModel";

/** Serialize a `code_fence` PM node back to fenced Markdown. */
export function pmCodeFenceToMarkdown(node: ProseMirrorNode): string {
  const content = node.textContent;
  const rawSource = String(node.attrs.rawSource ?? "");
  // Untouched blocks keep their authored bytes exactly — including info-string
  // spacing the normalized regeneration would not reproduce. The guard requires
  // every serialized dimension to still match the parsed raw, so a stale attr
  // (e.g. a language changed by a compact-control transaction that the fast
  // path did not consume) can never emit outdated source.
  if (rawSource && content === String(node.attrs.rawText ?? "")) {
    const parsed = parseCodeFenceSource(rawSource);
    // Meta may carry authored trailing whitespace that only the raw bytes keep;
    // compare trimmed so spacing variance never forces a rewrite of an untouched block.
    const nodeMeta = String(node.attrs.meta ?? "").trim();
    if (
      parsed &&
      normalizeFenceContent(parsed.content, String(node.attrs.contentIndent ?? "")) === content &&
      parsed.language === String(node.attrs.language ?? "") &&
      parsed.meta.trim() === nodeMeta &&
      parsed.fenceChar === String(node.attrs.fenceChar ?? "`") &&
      parsed.fenceLength === Number(node.attrs.fenceLength ?? 3) &&
      parsed.fenceIndent === String(node.attrs.fenceIndent ?? "") &&
      parsed.openingSuffix === String(node.attrs.openingSuffix ?? "\n") &&
      parsed.trailingNewline === Boolean(node.attrs.trailingNewline ?? true)
    ) {
      return rawSource;
    }
  }
  const meta = String(node.attrs.meta ?? "");
  const fenceChar = String(node.attrs.fenceChar ?? "`");
  const fenceLength = Number(node.attrs.fenceLength ?? 3);
  return serializeCodeFence({
    language: String(node.attrs.language ?? ""),
    meta,
    fenceChar: fenceChar[0] ?? "`",
    fenceLength: Number.isFinite(fenceLength) && fenceLength >= 3 ? fenceLength : 3,
    fenceIndent: String(node.attrs.fenceIndent ?? ""),
    contentIndent: String(node.attrs.contentIndent ?? ""),
    openingSuffix: String(node.attrs.openingSuffix ?? "\n"),
    trailingNewline: Boolean(node.attrs.trailingNewline ?? true),
    content,
  });
}

function normalizeFenceContent(content: string, contentIndent: string): string {
  if (!contentIndent) return content;
  return content
    .split("\n")
    .map((line) => (line.startsWith(contentIndent) ? line.slice(contentIndent.length) : line))
    .join("\n");
}

export function pmDocToMarkdown(doc: ProseMirrorNode) {
  let markdown = "";
  let blockIndex = 0;
  doc.forEach((node) => {
    const storedPrefix = node.attrs.prefix ?? "";
    // Parsed blocks retain their exact source prefix. Blocks created inside ProseMirror (Enter,
    // splitBlock, pasted slices) have no source prefix, so synthesize Markdown's paragraph separator
    // instead of concatenating them and erasing the user's rendered-editor line breaks on save.
    const prefix = blockIndex > 0 && storedPrefix === "" ? "\n\n" : storedPrefix;
    markdown += prefix + pmBlockToMarkdown(node);
    blockIndex++;
  });
  return markdown + (doc.attrs.trailing ?? "");
}

export function pmBlockToMarkdown(node: ProseMirrorNode, preserveRaw = true) {
  if (node.type.name === "raw_block") return node.attrs.rawSource ?? "";
  if (node.type.name === "code_fence") return pmCodeFenceToMarkdown(node);
  if (node.type.name === "table") return pmTableToMarkdown(node);
  if (preserveRaw && canPreservePmRaw(node)) {
    return node.attrs.rawSource;
  }
  const rawSource = String(node.attrs.rawSource ?? "");
  const inline = pmInlineToMarkdown(node, rawSource);
  if (node.type.name === "heading") return headingToMarkdown(node, inline);
  if (node.type.name === "blockquote") return pmBlockquoteToMarkdown(node, preserveRaw);
  if (node.type.name === "bullet_list" || node.type.name === "ordered_list")
    return pmListToMarkdown(node, preserveRaw);
  if (node.type.name === "description_list") return pmDescriptionListToMarkdown(node);
  return inline;
}

/**
 * Emphasis/strong delimiter inferred PER SPAN, not per block: scanning a whole block for "the"
 * emphasis char is wrong whenever ordinary text also contains a bare `_`/`*` (snake_case
 * identifiers, `2 * 3`, globs, …) — that mis-infers the delimiter for spans that have nothing to
 * do with it. Instead, at the point a given em/strong run is serialized, look for the exact
 * delimiter that wraps THAT run's plain text in the block's `rawSource`. Ambiguous (both match)
 * or unrecoverable (edited text no longer appears verbatim, or no rawSource) always falls back
 * to the canonical marker so untouched, unambiguous spans round-trip and everything else is
 * conservative rather than guessed.
 */
function delimiterFor(
  text: string,
  rawSource: string,
  kind: "em" | "strong",
): "*" | "_" | "**" | "__" {
  if (!text || !rawSource) return kind === "strong" ? "**" : "*";
  const [primary, alternate] = kind === "strong" ? (["**", "__"] as const) : (["*", "_"] as const);
  const hasPrimary = rawSource.includes(`${primary}${text}${primary}`);
  const hasAlternate = rawSource.includes(`${alternate}${text}${alternate}`);
  if (hasPrimary || !hasAlternate) return primary;
  return alternate;
}

/**
 * Heading trivia inferred from the block's own `rawSource`: ATX (`#`) vs. setext (`===`/`---`
 * underline). Setext is only honored when it is still valid for the current level (1 or 2) and
 * the serialized text is a single line — otherwise it falls back to ATX rather than emit broken
 * Markdown.
 */
function headingToMarkdown(node: ProseMirrorNode, inline: string): string {
  const level = Number(node.attrs.level ?? 1);
  const setext = inferSetextStyle(String(node.attrs.rawSource ?? ""));
  if (setext && !inline.includes("\n") && inline.trim().length > 0) {
    if ((setext === "=" && level === 1) || (setext === "-" && level === 2)) {
      return `${inline}\n${setext.repeat(Math.max(inline.length, 1))}`;
    }
  }
  return `${"#".repeat(level)} ${inline}`;
}

function inferSetextStyle(rawSource: string): "=" | "-" | null {
  if (!rawSource || /^#{1,6}(\s|$)/.test(rawSource)) return null;
  const match = /\n[ \t]*(=+|-+)[ \t]*$/.exec(rawSource);
  if (!match) return null;
  return match[1]?.startsWith("=") ? "=" : "-";
}

/** Preserve source spelling only while PM content, marks, & attrs serialize identically. */
export function canPreservePmRaw(node: ProseMirrorNode): boolean {
  const rawSource = String(node.attrs.rawSource ?? "");
  if (!rawSource || node.textContent !== String(node.attrs.rawText ?? "")) return false;
  const serialized = pmBlockToMarkdown(node, false);
  return (
    serialized === rawSource || normalizeSoftWraps(serialized) === normalizeSoftWraps(rawSource)
  );
}

function pmBlockquoteToMarkdown(node: ProseMirrorNode, preserveRaw = true) {
  const markdown = childBlocksToMarkdown(node, preserveRaw).split("\n");
  return markdown.map((line) => (line ? `> ${line}` : ">")).join("\n");
}

/** Bullet char / ordered delimiter inferred from the list's own `rawSource`, read at the start
 * of the raw text so nested lists (which carry their own `rawSource`) are never mismatched. */
function inferListMarkerTrivia(
  node: ProseMirrorNode,
): { readonly bullet: "-" | "*" | "+" } | { readonly delimiter: "." | ")" } {
  const raw = String(node.attrs.rawSource ?? "");
  if (node.type.name === "ordered_list") {
    const match = /^[ \t]*\d+([.)])[ \t]/.exec(raw);
    return { delimiter: match?.[1] === ")" ? ")" : "." };
  }
  const match = /^[ \t]*([-*+])[ \t]/.exec(raw);
  const bullet = match?.[1];
  return { bullet: bullet === "*" || bullet === "+" ? bullet : "-" };
}

export function pmListToMarkdown(node: ProseMirrorNode, preserveRaw = true) {
  const ordered = node.type.name === "ordered_list";
  const trivia = inferListMarkerTrivia(node);
  let index = Number(node.attrs.start ?? 1);
  const lines: string[] = [];
  node.forEach((item) => {
    const marker =
      ordered && "delimiter" in trivia
        ? `${index++}${trivia.delimiter} `
        : "bullet" in trivia
          ? `${trivia.bullet} `
          : "- ";
    const itemLines = childBlocksToMarkdown(item, preserveRaw).split("\n");
    lines.push(marker + (itemLines[0] ?? ""));
    for (const line of itemLines.slice(1)) lines.push(`  ${line}`);
  });
  return lines.join("\n");
}

function childBlocksToMarkdown(node: ProseMirrorNode, preserveRaw = true) {
  const blocks: string[] = [];
  node.forEach((child) => {
    blocks.push(pmBlockToMarkdown(child, preserveRaw));
  });
  return blocks.join("\n");
}

function pmDescriptionListToMarkdown(node: ProseMirrorNode) {
  const lines: string[] = [];
  node.forEach((item) => {
    if (item.type.name !== "description_item") return;
    let termLine = "";
    let detailsLine = "";
    item.forEach((child) => {
      if (child.type.name === "description_term") {
        termLine = pmInlineToMarkdown(child);
      } else if (child.type.name === "description_details") {
        const detail = pmInlineToMarkdown(child);
        if (!detailsLine) detailsLine = detail;
        else detailsLine += `\n${detail}`;
      }
    });
    if (termLine) lines.push(termLine);
    if (detailsLine) lines.push(`:   ${detailsLine}`);
  });
  return lines.join("\n");
}

function pmInlineToMarkdown(node: ProseMirrorNode, rawSource = "") {
  let markdown = "";
  node.forEach((child) => {
    if (child.type.name === "wiki_link") {
      markdown += wikiLinkToMarkdown(child);
      return;
    }
    if (child.type.name === "citation") {
      const { key, locator } = child.attrs as { key: string; locator: string };
      markdown += locator ? `[@${key} ${locator}]` : `[@${key}]`;
      return;
    }
    if (child.type.name === "image") {
      markdown += imageNodeToMarkdown(child);
      return;
    }
    if (!child.isText) {
      markdown += child.textContent;
      return;
    }
    markdown += textWithMarksToMarkdown(child.text ?? "", child.marks, rawSource);
  });
  return markdown;
}

export function imageNodeToMarkdown(node: ProseMirrorNode): string {
  const { src, alt, title, rawSource, sourceFrom } = node.attrs as {
    src: string;
    alt: string;
    title: string | null;
    rawSource: string;
    sourceFrom: number;
  };
  if (rawSource && typeof sourceFrom === "number" && sourceFrom >= 0) {
    try {
      const parsed = parseImageRawSource(rawSource);
      if (
        parsed &&
        parsed.alt === (alt ?? "") &&
        parsed.url === (src ?? "") &&
        (parsed.title ?? null) === (title ?? null)
      ) {
        return rawSource;
      }
    } catch {}
  }
  const escapedAlt = escapeImageAlt(alt ?? "");
  const escapedUrl = escapeImageUrl(src ?? "");
  const titlePart = title ? ` "${escapeImageTitle(title)}"` : "";
  return `![${escapedAlt}](${escapedUrl}${titlePart})`;
}

function parseImageRawSource(
  raw: string,
): { alt: string; url: string; title: string | null } | null {
  const m = raw.match(/^!\[([^\]]*)\]\(\s*([^\s\)]+)(?:\s+"([^"]*)")?\s*\)$/);
  if (!m) return null;
  return { alt: m[1] ?? "", url: m[2] ?? "", title: m[3] ?? null };
}

function escapeImageAlt(alt: string): string {
  return alt.replace(/\\/g, "\\\\").replace(/\[/g, "\\[").replace(/\]/g, "\\]");
}

function escapeImageUrl(url: string): string {
  return url
    .replace(/\\/g, "\\\\")
    .replace(/ /g, "%20")
    .replace(/\(/g, "\\(")
    .replace(/\)/g, "\\)");
}

function escapeImageTitle(title: string): string {
  return title.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, " ");
}


export function pmTableToMarkdown(node: ProseMirrorNode): string {
  return serializeGfmTable(node);
}

function wikiLinkToMarkdown(node: ProseMirrorNode): string {
  const { target, hash, display } = node.attrs as { target: string; hash: string; display: string };
  const address = target ? (hash ? `${target}#${hash}` : target) : `#${hash}`;
  if (display) return `[[${address}|${display}]]`;
  return `[[${address}]]`;
}

function textWithMarksToMarkdown(text: string, marks: readonly Mark[], rawSource = "") {
  const codeMark = marks.find((mark) => mark.type.name === "code");
  if (codeMark) return inlineCodeToMarkdown(text);

  let markdown = text;
  if (marks.some((mark) => mark.type.name === "strong")) {
    const strong = delimiterFor(text, rawSource, "strong");
    markdown = `${strong}${markdown}${strong}`;
  }
  if (marks.some((mark) => mark.type.name === "em")) {
    const em = delimiterFor(text, rawSource, "em");
    markdown = `${em}${markdown}${em}`;
  }
  if (marks.some((mark) => mark.type.name === "delete")) markdown = `~~${markdown}~~`;
  if (marks.some((mark) => mark.type.name === "mark")) markdown = `==${markdown}==`;
  // abbr has no inline Markdown syntax; the abbreviation definition block preserves it.
  if (marks.some((mark) => mark.type.name === "abbr")) markdown = `${markdown}`;
  const linkMark = marks.find((mark) => mark.type.name === "link");
  if (linkMark) {
    const title = linkMark.attrs.title ? ` "${String(linkMark.attrs.title)}"` : "";
    markdown = `[${markdown}](${String(linkMark.attrs.href)}${title})`;
  }
  return markdown;
}

function inlineCodeToMarkdown(text: string) {
  const fence = text.includes("`") ? "``" : "`";
  return `${fence}${text}${fence}`;
}

export function normalizeSoftWraps(value: string): string {
  return value.replace(/\r?\n/g, " ");
}
