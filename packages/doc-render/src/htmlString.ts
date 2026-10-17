import type { RootContent } from "mdast";
import { buildHeadingSlugs } from "./headingSlugs.js";
import { collectFootnotes } from "./footnotes.js";
import { parseToTree, type ParseOptions } from "./parseMarkdown.js";
import { resolveImageSource, safeLinkHref, sanitizeHtml, type LinkPolicy, type SanitizeOptions } from "./security.js";
import type { CslEntry } from "./citations/types.js";
import { formatCitation } from "./citations/formatter.js";

export interface MarkdownHtmlOptions {
  parseOptions?: ParseOptions;
  linkPolicy?: LinkPolicy;
  htmlPolicy?: SanitizeOptions;
  bibliography?: Map<string, CslEntry>;
}

const esc = (value: string) => value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");

/** Serialize Markdown to an HTML string (export, clipboard, print) with the same link, image and
 * raw-HTML policy as `MarkdownRenderer`. Every text and attribute value is escaped; raw HTML goes
 * through `sanitizeHtml` (escaped to inert text when there is no DOM). Math, citations and
 * footnote bodies render as plain text; wiki links and unsafe links lose their anchor. */
export function renderMarkdownToHtml(source: string, options: MarkdownHtmlOptions = {}): string {
  const tree = parseToTree(source, options.parseOptions);
  const slugs = buildHeadingSlugs(tree);
  const footnotes = collectFootnotes(tree);
  const bibliography = options.bibliography ?? new Map<string, CslEntry>();
  const linkPolicy = options.linkPolicy ?? {};
  const htmlPolicy = { ...options.htmlPolicy, ...linkPolicy };

  const many = (nodes: RootContent[]): string => nodes.map(one).join("");
  function one(node: RootContent): string {
    const raw = node as any;
    const kids = raw.children as RootContent[] | undefined;
    switch (raw.type) {
      case "text": return esc(raw.value);
      case "highlight": return `<mark>${many(kids ?? [])}</mark>`;
      case "paragraph": return `<p>${many(kids ?? [])}</p>`;
      case "heading": { const id = slugs.get(node as never); return `<h${raw.depth}${id ? ` id="${esc(id)}"` : ""}>${many(kids ?? [])}</h${raw.depth}>`; }
      case "emphasis": return `<em>${many(kids ?? [])}</em>`;
      case "strong": return `<strong>${many(kids ?? [])}</strong>`;
      case "delete": return `<del>${many(kids ?? [])}</del>`;
      case "inlineCode": return `<code>${esc(raw.value)}</code>`;
      case "code": return `<pre><code${raw.lang && /^[\w+#.-]+$/.test(raw.lang) ? ` class="language-${esc(raw.lang)}"` : ""}>${esc(raw.value)}</code></pre>`;
      case "blockquote": return `<blockquote>${many(kids ?? [])}</blockquote>`;
      case "list": { const tag = raw.ordered ? "ol" : "ul"; return `<${tag}${raw.ordered && raw.start != null && raw.start !== 1 ? ` start="${Number(raw.start) | 0}"` : ""}>${many(kids ?? [])}</${tag}>`; }
      case "listItem": return `<li>${many(kids ?? [])}</li>`;
      case "link": {
        const href = safeLinkHref(raw.url, linkPolicy);
        return href ? `<a href="${esc(href)}" rel="noreferrer noopener" target="_blank">${many(kids ?? [])}</a>` : `<span>${many(kids ?? [])}</span>`;
      }
      case "image": {
        const src = resolveImageSource(raw.url, options.htmlPolicy);
        return src ? `<img src="${esc(src)}" alt="${esc(raw.alt ?? "")}"${raw.title ? ` title="${esc(raw.title)}"` : ""}>` : `<span>${esc(raw.alt ?? "")}</span>`;
      }
      case "thematicBreak": return "<hr>";
      case "break": return "<br>";
      case "html": return sanitizeHtml(raw.value, htmlPolicy);
      case "table": {
        const [head, ...rows] = (raw.children ?? []) as any[];
        const cells = (row: any, tag: string) => (row?.children ?? []).map((cell: any) => `<${tag}>${many(cell.children)}</${tag}>`).join("");
        return `<table><thead><tr>${cells(head, "th")}</tr></thead><tbody>${rows.map((row) => `<tr>${cells(row, "td")}</tr>`).join("")}</tbody></table>`;
      }
      case "footnoteReference": { const note = footnotes.get(raw.identifier); return note ? `<sup>${esc(String(note.num))}</sup>` : ""; }
      case "math": return `<pre>${esc(raw.value)}</pre>`;
      case "inlineMath": return `<code>${esc(raw.value)}</code>`;
      case "citation": { const entry = bibliography.get(raw.key); return esc(entry ? formatCitation(entry, raw.locator) : `[${raw.key}]`); }
      case "definition": case "footnoteDefinition": case "yaml": case "toml": case "bibliography": return "";
      default: return kids?.length ? many(kids) : "";
    }
  }
  return many(tree.children);
}
