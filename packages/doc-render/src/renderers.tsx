import { createElement, type ReactNode } from "react";
import type { Root, RootContent } from "mdast";
import { buildHeadingSlugs } from "./headingSlugs.js";
import { collectFootnotes } from "./footnotes.js";
import { nodeText } from "./detectors/nodeText.js";
import { parseToTree } from "./parseMarkdown.js";
import { safeImageHref, safeLinkHref, sanitizeHtml } from "./security.js";
import type { CslEntry } from "./citations/types.js";
import { formatBibliographyEntry, formatCitation } from "./citations/formatter.js";

export interface MarkdownRendererProps {
  source: string;
  className?: string;
  onOpenLink?: (href: string) => void;
  bibliography?: Map<string, CslEntry>;
}

export interface HtmlRendererProps {
  html: string;
  className?: string;
  safeMode?: boolean;
}

function textContent(children: RootContent[]): string { return children.map((child) => nodeText(child as RootContent)).join(""); }

function renderChildren(children: RootContent[], context: RenderContext): ReactNode[] {
  return children.map((child, index) => renderNode(child, `${context.key}:${index}`, context));
}

interface RenderContext {
  headingSlugs: ReturnType<typeof buildHeadingSlugs>;
  footnotes: ReturnType<typeof collectFootnotes>;
  onOpenLink?: (href: string) => void;
  bibliography: Map<string, CslEntry>;
  key: string;
}

function renderNode(node: RootContent, key: string, parent: RenderContext): ReactNode {
  const raw = node as any;
  const context = { ...parent, key };
  const children = (node as { children?: RootContent[] }).children ?? [];
  switch (raw.type) {
    case "text": return raw.value;
    case "paragraph": return <p key={key}>{renderChildren(children, context)}</p>;
    case "heading": return createElement(`h${raw.depth}`, { key, id: parent.headingSlugs.get(node as never) }, renderChildren(children, context));
    case "emphasis": return <em key={key}>{renderChildren(children, context)}</em>;
    case "strong": return <strong key={key}>{renderChildren(children, context)}</strong>;
    case "delete": return <del key={key}>{renderChildren(children, context)}</del>;
    case "inlineCode": return <code key={key}>{raw.value}</code>;
    case "code": return <pre key={key} className="rk-doc-code"><code>{raw.value}</code></pre>;
    case "blockquote": return <blockquote key={key}>{renderChildren(children, context)}</blockquote>;
    case "list": return createElement(raw.ordered ? "ol" : "ul", { key, start: raw.ordered && raw.start !== 1 ? raw.start : undefined }, renderChildren(children, context));
    case "listItem": return <li key={key}>{renderChildren(children, context)}</li>;
    case "link": {
      const href = safeLinkHref(raw.url);
      if (!href) return <span key={key}>{renderChildren(children, context)}</span>;
      return <a key={key} href={href} rel="noreferrer noopener" target="_blank" onClick={(event) => { if (parent.onOpenLink) { event.preventDefault(); parent.onOpenLink(href); } }}>{renderChildren(children, context)}</a>;
    }
    case "image": {
      const src = safeImageHref(raw.url);
      return src ? <img key={key} src={src} alt={raw.alt ?? ""} title={raw.title ?? undefined} loading="lazy" /> : <span key={key}>{raw.alt ?? ""}</span>;
    }
    case "thematicBreak": return <hr key={key} />;
    case "break": return <br key={key} />;
    case "html": return <span key={key} dangerouslySetInnerHTML={{ __html: sanitizeHtml(raw.value) }} />;
    case "table": return <table key={key}><thead><tr>{raw.children[0]?.children.map((cell: any, i: number) => <th key={i}>{renderChildren(cell.children, context)}</th>)}</tr></thead><tbody>{raw.children.slice(1).map((row: any, i: number) => <tr key={i}>{row.children.map((cell: any, j: number) => <td key={j}>{renderChildren(cell.children, context)}</td>)}</tr>)}</tbody></table>;
    case "footnoteReference": {
      const footnote = parent.footnotes.get(raw.identifier);
      return footnote ? <sup key={key}><a href={`#fn-${raw.identifier}`} id={`fnref-${raw.identifier}`}>{footnote.num}</a></sup> : null;
    }
    case "math": return <pre key={key} className="rk-doc-math">{raw.value}</pre>;
    case "inlineMath": return <code key={key} className="rk-doc-math-inline">{raw.value}</code>;
    case "definition": case "footnoteDefinition": case "yaml": case "toml": return null;
    case "imageReference": case "linkReference": return <span key={key}>{textContent(children)}</span>;
    case "citation": {
      const entry = parent.bibliography.get((node as unknown as { key: string }).key);
      return <span key={key}>{entry ? formatCitation(entry, (node as unknown as { locator?: string }).locator) : `[${(node as unknown as { key: string }).key}]`}</span>;
    }
    case "bibliography": return <Bibliography key={key} entries={[...parent.bibliography.values()]} />;
    default: return children.length ? <span key={key}>{renderChildren(children, context)}</span> : null;
  }
}

function Bibliography({ entries }: { entries: CslEntry[] }) {
  return <section className="rk-doc-bibliography"><h2>References</h2><ol>{entries.map((entry) => <li key={entry.id}>{formatBibliographyEntry(entry)}</li>)}</ol></section>;
}

export function MarkdownRenderer({ source, className, onOpenLink, bibliography = new Map() }: MarkdownRendererProps) {
  const tree = parseToTree(source);
  const headingSlugs = buildHeadingSlugs(tree);
  const footnotes = collectFootnotes(tree);
  const context: RenderContext = { headingSlugs, footnotes, onOpenLink, bibliography, key: "root" };
  const content = renderChildren(tree.children, context);
  if (footnotes.size) content.push(<section key="footnotes" className="rk-doc-footnotes"><h2>Notes</h2><ol>{[...footnotes.entries()].map(([id, footnote]) => <li key={id} id={`fn-${id}`}>{renderChildren(footnote.children, { ...context, key: `fn:${id}` })}</li>)}</ol></section>);
  return <article className={`rk-doc-markdown ${className ?? ""}`}>{content}</article>;
}

export function HtmlRenderer({ html, className, safeMode = true }: HtmlRendererProps) {
  const content = safeMode ? sanitizeHtml(html) : html;
  return <article className={`rk-doc-html ${className ?? ""}`} dangerouslySetInnerHTML={{ __html: content }} />;
}

export function renderMarkdown(source: string, options?: Omit<MarkdownRendererProps, "source">): ReactNode {
  return <MarkdownRenderer source={source} {...options} />;
}

export function renderHtml(html: string, options?: Omit<HtmlRendererProps, "html">): ReactNode {
  return <HtmlRenderer html={html} {...options} />;
}
