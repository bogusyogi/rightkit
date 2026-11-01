import { renderMarkdownToHtml } from "@rightkit/doc-render";
import { parseToTree } from "./parseMarkdown.js";
import { nodeText } from "./semanticBlocks.js";

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] ?? c,
  );
}

// the host app own export look; RightKit supplies only the Markdown-to-HTML body.
const EXPORT_CSS = `
  :root { color-scheme: light dark; }
  body { max-width: 44rem; margin: 3rem auto; padding: 0 1.25rem; font-family: Georgia, "Times New Roman", serif; font-size: 18px; line-height: 1.7; color: #1a1a1a; }
  h1, h2, h3, h4 { font-family: system-ui, sans-serif; line-height: 1.25; }
  pre { background: #f4f1ea; padding: 1rem; border-radius: 6px; overflow: auto; }
  code { font-family: ui-monospace, "SFMono-Regular", Menlo, monospace; font-size: 0.9em; }
  pre code { font-size: 0.85em; }
  blockquote { margin: 1.2em 0; padding: 0.6em 1em; border: 1px solid #c9bfa9; border-radius: 6px; color: #4a4438; }
  table { border-collapse: collapse; }
  th, td { border: 1px solid #d8d0c0; padding: 0.4em 0.7em; }
  img { max-width: 100%; height: auto; }
  a { color: #8f4d2e; }
  @media (prefers-color-scheme: dark) { body { background: #1a1a1a; color: #e7e2d8; } pre { background: #26241f; } a { color: #d98a5e; } }
`;

/** Render Markdown source to a standalone, self-styled HTML document. */
export function markdownToHtml(source: string, title = "Document"): string {
  const body = markdownToHtmlFragment(source);
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>${EXPORT_CSS}</style>
</head>
<body>
${body}
</body>
</html>
`;
}

/** Render Markdown source to an HTML body fragment (no doctype/head/style) — for clipboard rich-text,
 *  where the paste target (email, docs, chat) applies its own styling to the semantic tags. */
export function markdownToHtmlFragment(source: string): string {
  return renderMarkdownToHtml(source, { parseOptions: { highlight: true } });
}

/** Strip Markdown to readable plain text (top-level blocks separated by blank lines). */
export function markdownToPlainText(source: string): string {
  const tree = parseToTree(source);
  const blocks = tree.children.map((node) => nodeText(node).trim()).filter(Boolean);
  return `${blocks.join("\n\n")}\n`;
}
