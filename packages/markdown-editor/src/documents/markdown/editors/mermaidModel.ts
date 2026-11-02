/**
 * Mermaid editor — pure model.
 *
 * Reads a `mermaid` fenced code block and provides operations.
 * Validation runs through {@link analyzeMarkdown}'s
 * `validateMermaid` async validator; this model is pure and never
 * calls the validator directly.
 */

import { parseMarkdownCodeBlock } from "./codeBlockModel.js";

export interface MarkdownMermaidModel {
  readonly content: string;
  readonly language: string;
  readonly fenceChar: "`" | "~";
  readonly fenceLength: number;
  readonly fenceIndent: string;
  readonly lineEnding: "\n" | "\r\n";
  readonly closingFence: string | null;
  readonly trailingLineEnding: boolean;
  readonly originalSource: string;
  readonly originalContent: string;
}

/**
 * Parse a Mermaid block. Returns `null` when the block is not a
 * `mermaid` fenced code block.
 */
export function parseMarkdownMermaid(
  source: string,
  from: number,
  to: number,
): MarkdownMermaidModel | null {
  const parsed = parseMarkdownCodeBlock(source, from, to);
  if (!parsed || parsed.style !== "fenced") return null;
  if (parsed.language.toLowerCase() !== "mermaid" && parsed.language.toLowerCase() !== "mmd")
    return null;
  return {
    content: parsed.content,
    language: parsed.language,
    fenceChar: parsed.fenceChar ?? "`",
    fenceLength: parsed.fenceLength,
    fenceIndent: parsed.fenceIndent,
    lineEnding: parsed.lineEnding,
    closingFence: parsed.closingFence,
    trailingLineEnding: parsed.trailingLineEnding,
    originalSource: parsed.originalSource,
    originalContent: parsed.originalContent,
  };
}

/**
 * Serialize the Mermaid block back to Markdown.
 */
export function stringifyMarkdownMermaid(model: MarkdownMermaidModel): string {
  if (model.content === model.originalContent) return model.originalSource;
  const fence = model.fenceChar.repeat(Math.max(model.fenceLength, 3));
  const opening = `${model.fenceIndent}${fence}${model.language || "mermaid"}${model.lineEnding}`;
  const body = `${opening}${model.content}`;
  if (!model.closingFence) return `${body}${model.trailingLineEnding ? model.lineEnding : ""}`;
  return `${body}${model.lineEnding}${model.closingFence}${model.trailingLineEnding ? model.lineEnding : ""}`;
}

/**
 * Replace the Mermaid block content.
 */
export function setMermaidContent(
  model: MarkdownMermaidModel,
  content: string,
): MarkdownMermaidModel {
  return { ...model, content };
}
