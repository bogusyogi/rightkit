/**
 * Markdown insertion templates.
 *
 * Canonical snippets the insert menu drops at the cursor. Each
 * template exposes both the literal snippet (with placeholders
 * for the caret) and the kind tag the host dispatches on.
 */

export type MarkdownInsertKind =
  | "heading"
  | "bulleted-list"
  | "numbered-list"
  | "quote"
  | "divider"
  | "table"
  | "image"
  | "code"
  | "mermaid"
  | "math-block"
  | "math-inline"
  | "task-list"
  | "footnote";

export interface MarkdownInsertTemplate {
  readonly kind: MarkdownInsertKind;
  readonly label: string;
  readonly snippet: string;
  /** Cursor offset within the snippet after insertion. */
  readonly cursorOffset: number;
}

const HEADING_SNIPPET = "## Heading\n";
const BULLETED_LIST_SNIPPET = "- Item one\n- Item two\n- Item three\n";
const NUMBERED_LIST_SNIPPET = "1. Item one\n2. Item two\n3. Item three\n";
const QUOTE_SNIPPET = "> Quote\n";
const DIVIDER_SNIPPET = "---\n";
const TABLE_SNIPPET =
  "| Column A | Column B |\n| -------- | -------- |\n| A1       | B1       |\n| A2       | B2       |\n";
const IMAGE_SNIPPET = "![alt text](image-path-or-url)";
const CODE_SNIPPET = "```ts\n\n```\n";
const MERMAID_SNIPPET = "```mermaid\ngraph TD; A --> B;\n```\n";
const MATH_BLOCK_SNIPPET = "$$\n\n$$\n";
const MATH_INLINE_SNIPPET = "$a^2 + b^2 = c^2$";
const TASK_LIST_SNIPPET = "- [ ] task 1\n- [ ] task 2\n- [ ] task 3\n";
const FOOTNOTE_SNIPPET = "Reference[^1].\n\n[^1]: Footnote body.\n";

export const MARKDOWN_INSERT_TEMPLATES: readonly MarkdownInsertTemplate[] = [
  {
    kind: "heading",
    label: "Heading",
    snippet: HEADING_SNIPPET,
    cursorOffset: HEADING_SNIPPET.indexOf("Heading"),
  },
  {
    kind: "bulleted-list",
    label: "Bulleted list",
    snippet: BULLETED_LIST_SNIPPET,
    cursorOffset: BULLETED_LIST_SNIPPET.indexOf("Item one"),
  },
  {
    kind: "numbered-list",
    label: "Numbered list",
    snippet: NUMBERED_LIST_SNIPPET,
    cursorOffset: NUMBERED_LIST_SNIPPET.indexOf("Item one"),
  },
  {
    kind: "quote",
    label: "Quote",
    snippet: QUOTE_SNIPPET,
    cursorOffset: QUOTE_SNIPPET.indexOf("Quote"),
  },
  {
    kind: "divider",
    label: "Divider",
    snippet: DIVIDER_SNIPPET,
    cursorOffset: DIVIDER_SNIPPET.length,
  },
  {
    kind: "table",
    label: "Table",
    snippet: TABLE_SNIPPET,
    cursorOffset: TABLE_SNIPPET.indexOf("Column"),
  },
  {
    kind: "image",
    label: "Image",
    snippet: IMAGE_SNIPPET,
    cursorOffset: IMAGE_SNIPPET.indexOf("alt text"),
  },
  {
    kind: "code",
    label: "Code block",
    snippet: CODE_SNIPPET,
    cursorOffset: CODE_SNIPPET.indexOf("\n") + 1,
  },
  {
    kind: "mermaid",
    label: "Mermaid diagram",
    snippet: MERMAID_SNIPPET,
    cursorOffset: MERMAID_SNIPPET.indexOf("graph"),
  },
  {
    kind: "math-block",
    label: "Math block",
    snippet: MATH_BLOCK_SNIPPET,
    cursorOffset: MATH_BLOCK_SNIPPET.indexOf("\n") + 1,
  },
  { kind: "math-inline", label: "Math inline", snippet: MATH_INLINE_SNIPPET, cursorOffset: 1 },
  {
    kind: "task-list",
    label: "Task list",
    snippet: TASK_LIST_SNIPPET,
    cursorOffset: TASK_LIST_SNIPPET.indexOf("task 1"),
  },
  {
    kind: "footnote",
    label: "Footnote",
    snippet: FOOTNOTE_SNIPPET,
    cursorOffset: FOOTNOTE_SNIPPET.indexOf("[^"),
  },
];

/**
 * Look up a template by kind.
 */
export function getInsertTemplate(kind: MarkdownInsertKind): MarkdownInsertTemplate {
  const found = MARKDOWN_INSERT_TEMPLATES.find((t) => t.kind === kind);
  if (!found) throw new Error(`Unknown insert kind: ${kind}`);
  return found;
}
