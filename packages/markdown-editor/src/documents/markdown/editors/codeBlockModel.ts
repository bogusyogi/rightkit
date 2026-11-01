/**
 * Code-block editor — pure model.
 *
 * Reads a fenced code block and provides operations that
 * round-trip to a stable textual form. Language is preserved.
 */

export interface MarkdownCodeBlockModel {
  readonly language: string;
  readonly content: string;
  readonly meta: string | null;
  /** Authored fence trivia is retained for malformed, tilde, & indented blocks. */
  readonly style: "fenced" | "indented";
  readonly fenceChar: "`" | "~" | null;
  readonly fenceLength: number;
  readonly fenceIndent: string;
  readonly closingFence: string | null;
  readonly lineEnding: "\n" | "\r\n";
  readonly trailingLineEnding: boolean;
  readonly originalSource: string;
  readonly originalLanguage: string;
  readonly originalContent: string;
  readonly originalMeta: string | null;
}

/**
 * Parse a fenced code block between `[from, to)`. Returns `null`
 * if the source is not a valid code block.
 */
export function parseMarkdownCodeBlock(
  source: string,
  from: number,
  to: number,
): MarkdownCodeBlockModel | null {
  const slice = source.slice(from, to);
  const lineEnding: "\n" | "\r\n" = slice.includes("\r\n") ? "\r\n" : "\n";
  const lines = splitLines(slice);
  const first = lines[0];
  if (!first) return null;

  const fenced = /^(?<indent>[ \t]{0,3})(?<fence>`{3,}|~{3,})(?<info>[^\r\n]*)$/.exec(first.text);
  if (fenced?.groups) {
    const indent = fenced.groups.indent ?? "";
    const marker = fenced.groups.fence ?? "```";
    const info = fenced.groups.info ?? "";
    const { language, meta } = splitInfo(info);
    const closingPattern = new RegExp(
      `^(?<indent>[ \\t]*)${marker[0] === "`" ? "\\`" : "~"}{${marker.length},}[ \\t]*$`,
    );
    const closingIndex = lines.findIndex(
      (line, index) => index > 0 && closingPattern.test(line.text),
    );
    const contentLines = closingIndex >= 0 ? lines.slice(1, closingIndex) : lines.slice(1);
    const content = contentLines.map((line) => line.text).join(lineEnding);
    const closing = closingIndex >= 0 ? (lines[closingIndex]?.text ?? null) : null;
    const trailingLineEnding =
      closingIndex >= 0
        ? (lines[closingIndex]?.ending ?? "") !== ""
        : (lines[lines.length - 1]?.ending ?? "") !== "";
    const model = {
      language,
      content,
      meta,
      style: "fenced" as const,
      fenceChar: marker[0] as "`" | "~",
      fenceLength: marker.length,
      fenceIndent: indent,
      closingFence: closing,
      lineEnding,
      trailingLineEnding,
      originalSource: slice,
    };
    return {
      ...model,
      originalLanguage: model.language,
      originalContent: model.content,
      originalMeta: model.meta,
    };
  }

  // Markdown's indented code form has no language/fence metadata. Keep its four-space
  // (or tab) prefix when content is edited so first-character edits cannot de-indent it.
  if (/^(?: {4}|\t)/.test(first.text)) {
    const indent = first.text.startsWith("\t") ? "\t" : "    ";
    const contentLines = lines.map((line) => {
      if (line.text === "") return "";
      return line.text.startsWith(indent) ? line.text.slice(indent.length) : line.text;
    });
    const trailingLineEnding = (lines[lines.length - 1]?.ending ?? "") !== "";
    const content = contentLines.join(lineEnding);
    return {
      language: "",
      content,
      meta: null,
      style: "indented",
      fenceChar: null,
      fenceLength: 0,
      fenceIndent: indent,
      closingFence: null,
      lineEnding,
      trailingLineEnding,
      originalSource: slice,
      originalLanguage: "",
      originalContent: content,
      originalMeta: null,
    };
  }
  return null;
}

/**
 * Serialize the code block back to Markdown. The opening fence
 * includes the language and the optional meta string.
 */
export function stringifyMarkdownCodeBlock(model: MarkdownCodeBlockModel): string {
  if (
    model.language === model.originalLanguage &&
    model.content === model.originalContent &&
    model.meta === model.originalMeta
  ) {
    return model.originalSource;
  }
  if (model.style === "indented") {
    const body = model.content
      .split("\n")
      .map((line) => (line ? `${model.fenceIndent}${line}` : line))
      .join(model.lineEnding);
    return `${body}${model.trailingLineEnding ? model.lineEnding : ""}`;
  }
  const fence = (model.fenceChar ?? "`").repeat(Math.max(model.fenceLength, 3));
  const langPart = model.language ? model.language : "";
  const metaPart = model.meta ? ` ${model.meta}` : "";
  const body = `${model.fenceIndent}${fence}${langPart}${metaPart}${model.lineEnding}${model.content}`;
  if (!model.closingFence) return `${body}${model.trailingLineEnding ? model.lineEnding : ""}`;
  return `${body}${model.lineEnding}${model.closingFence}${model.trailingLineEnding ? model.lineEnding : ""}`;
}

/**
 * Update the language while keeping content and meta stable.
 */
export function setCodeBlockLanguage(
  model: MarkdownCodeBlockModel,
  language: string,
): MarkdownCodeBlockModel {
  return { ...model, language };
}

/**
 * Replace the code-block content. Returns a new model with the
 * trailing newline preserved.
 */
export function setCodeBlockContent(
  model: MarkdownCodeBlockModel,
  content: string,
): MarkdownCodeBlockModel {
  return { ...model, content };
}

function splitInfo(info: string): { language: string; meta: string | null } {
  const trimmed = info.trim();
  if (!trimmed) return { language: "", meta: null };
  const spaceAt = trimmed.search(/\s/);
  if (spaceAt < 0) return { language: trimmed, meta: null };
  return { language: trimmed.slice(0, spaceAt), meta: trimmed.slice(spaceAt).trim() || null };
}

function splitLines(source: string): Array<{ text: string; ending: string }> {
  const lines: Array<{ text: string; ending: string }> = [];
  const re = /([^\r\n]*)(\r\n|\n|$)/g;
  let match = re.exec(source);
  while (match !== null) {
    const text = match[1] ?? "";
    const ending = match[2] ?? "";
    if (ending === "" && match.index === source.length) break;
    lines.push({ text, ending });
    if (ending === "") break;
    match = re.exec(source);
  }
  return lines;
}
