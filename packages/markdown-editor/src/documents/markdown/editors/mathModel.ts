/**
 * Math editor — pure model.
 *
 * Reads a `$...$` (inline) or `$$...$$` (block) math range and
 * provides operations that round-trip to a stable textual form.
 * The serializer strips a single trailing newline to match
 * canonical form.
 */

export type MathMode = "inline" | "block";

export interface MarkdownMathModel {
  readonly mode: MathMode;
  readonly value: string;
}

/**
 * Parse a math range. Returns `null` if the source is not a
 * recognized math expression.
 */
export function parseMarkdownMath(
  source: string,
  from: number,
  to: number,
): MarkdownMathModel | null {
  const slice = source.slice(from, to);
  const blockMatch = /^\$\$([\s\S]*?)\$\$\s*$/m.exec(slice);
  if (blockMatch) {
    return { mode: "block", value: blockMatch[1] ?? "" };
  }
  const inlineMatch = /^\$([^\$\n]+)\$\s*$/m.exec(slice);
  if (inlineMatch) {
    return { mode: "inline", value: inlineMatch[1] ?? "" };
  }
  return null;
}

/**
 * Serialize the math expression back to Markdown.
 */
export function stringifyMarkdownMath(model: MarkdownMathModel): string {
  if (model.mode === "block") return `$$${model.value}$$\n`;
  return `$${model.value}$`;
}

/**
 * Update the value while preserving the mode.
 */
export function setMathValue(model: MarkdownMathModel, value: string): MarkdownMathModel {
  return { ...model, value };
}
