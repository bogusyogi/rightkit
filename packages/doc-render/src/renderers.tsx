import { createElement, Fragment, type ReactNode } from "react";
import type { Root, RootContent } from "mdast";
import { buildHeadingSlugs } from "./headingSlugs.js";
import { collectFootnotes } from "./footnotes.js";
import { nodeText } from "./detectors/nodeText.js";
import { parseToTree, type ParseOptions } from "./parseMarkdown.js";
import { resolveImageSource, safeLinkHref, sanitizeHtml, type LinkPolicy, type SanitizeOptions } from "./security.js";
import type { CslEntry } from "./citations/types.js";
import { formatBibliographyEntry, formatCitation } from "./citations/formatter.js";

/** Host overrides for node rendering. Each override receives already-rendered children; `link`
 * is called only for hrefs that passed the link policy. An override returning `undefined` falls back to the default element. */
export interface MarkdownComponents {
  link?: (props: { href: string; children: ReactNode; explicit: boolean }) => ReactNode | undefined;
  /** Plain text node. Return `undefined` to render the text unchanged. */
  text?: (value: string, key: string) => ReactNode | undefined;
  inlineCode?: (props: { value: string }) => ReactNode | undefined;
  code?: (props: { lang?: string; value: string }) => ReactNode | undefined;
  table?: (props: { head: ReactNode[]; rows: ReactNode[][]; align: Array<"left" | "right" | "center" | null> }) => ReactNode | undefined;
  heading?: (props: { depth: number; id?: string; children: ReactNode }) => ReactNode | undefined;
  listItem?: (props: { checked?: boolean | null; children: ReactNode }) => ReactNode | undefined;
  paragraph?: (props: { children: ReactNode }) => ReactNode | undefined;
  blockquote?: (props: { children: ReactNode }) => ReactNode | undefined;
}

/** Extra class names for default elements. */
export type MarkdownClassNames = Partial<Record<"paragraph" | "heading" | "blockquote" | "list" | "listItem" | "link" | "code" | "inlineCode" | "table" | "highlight", string>>;

export interface MarkdownRendererProps {
  source: string;
  className?: string;
  onOpenLink?: (href: string) => void;
  bibliography?: Map<string, CslEntry>;
  /** Parser switches; see `ParseOptions`. */
  parseOptions?: ParseOptions;
  components?: MarkdownComponents;
  classNames?: MarkdownClassNames;
  /** Shorthand for `components.text`; ignored when `components.text` is set. */
  onText?: (value: string, key: string) => ReactNode | undefined;
  /** Link policy additions (tel:, relative hrefs). Default: web and mail only. */
  linkPolicy?: LinkPolicy;
  /** Sanitizer policy for inline raw HTML. */
  htmlPolicy?: SanitizeOptions;
}

export interface HtmlRendererProps {
  html: string;
  className?: string;
  safeMode?: boolean;
  htmlPolicy?: SanitizeOptions;
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
  components: MarkdownComponents;
  classNames: MarkdownClassNames;
  linkPolicy: LinkPolicy;
  htmlPolicy: SanitizeOptions;
  key: string;
}

const cls = (name: string | undefined) => (name ? name : undefined);
function over<T>(result: ReactNode | undefined, fallback: () => T, key: string): ReactNode {
  return result === undefined ? (fallback() as ReactNode) : <Fragment key={key}>{result}</Fragment>;
}

function renderNode(node: RootContent, key: string, parent: RenderContext): ReactNode {
  const raw = node as any;
  const context = { ...parent, key };
  const children = (node as { children?: RootContent[] }).children ?? [];
  switch (raw.type) {
    case "text": {
      const hooked = parent.components.text?.(raw.value, key);
      return hooked === undefined ? raw.value : <Fragment key={key}>{hooked}</Fragment>;
    }
    case "highlight": return <mark key={key} className={cls(parent.classNames.highlight)}>{renderChildren(children, context)}</mark>;
    case "paragraph": return over(parent.components.paragraph?.({ children: renderChildren(children, context) }), () => <p key={key} className={cls(parent.classNames.paragraph)}>{renderChildren(children, context)}</p>, key);
    case "heading": {
      const id = parent.headingSlugs.get(node as never);
      return over(parent.components.heading?.({ depth: raw.depth, id, children: renderChildren(children, context) }), () => createElement(`h${raw.depth}`, { key, id, className: cls(parent.classNames.heading) }, renderChildren(children, context)), key);
    }
    case "emphasis": return <em key={key}>{renderChildren(children, context)}</em>;
    case "strong": return <strong key={key}>{renderChildren(children, context)}</strong>;
    case "delete": return <del key={key}>{renderChildren(children, context)}</del>;
    case "inlineCode": return over(parent.components.inlineCode?.({ value: raw.value }), () => <code key={key} className={cls(parent.classNames.inlineCode)}>{raw.value}</code>, key);
    case "code": return over(parent.components.code?.({ lang: raw.lang ?? undefined, value: raw.value }), () => <pre key={key} className={["rk-doc-code", parent.classNames.code].filter(Boolean).join(" ")}><code>{raw.value}</code></pre>, key);
    case "blockquote": return over(parent.components.blockquote?.({ children: renderChildren(children, context) }), () => <blockquote key={key} className={cls(parent.classNames.blockquote)}>{renderChildren(children, context)}</blockquote>, key);
    case "list": return createElement(raw.ordered ? "ol" : "ul", { key, className: cls(parent.classNames.list), start: raw.ordered && raw.start !== 1 ? raw.start : undefined }, renderChildren(children, context));
    case "listItem": return over(parent.components.listItem?.({ checked: raw.checked, children: renderChildren(children, context) }), () => <li key={key} className={cls(parent.classNames.listItem)}>{renderChildren(children, context)}</li>, key);
    case "link": {
      const href = safeLinkHref(raw.url, parent.linkPolicy);
      if (!href) return <span key={key}>{renderChildren(children, context)}</span>;
      const rendered = renderChildren(children, context);
      const custom = parent.components.link?.({ href, children: rendered, explicit: !raw.data?.autolink });
      if (custom !== undefined) return <Fragment key={key}>{custom}</Fragment>;
      return <a key={key} className={cls(parent.classNames.link)} href={href} rel="noreferrer noopener" target="_blank" onClick={(event) => { if (parent.onOpenLink) { event.preventDefault(); parent.onOpenLink(href); } }}>{rendered}</a>;
    }
    case "image": {
      const src = resolveImageSource(raw.url, parent.htmlPolicy);
      return src ? <img key={key} src={src} alt={raw.alt ?? ""} title={raw.title ?? undefined} loading="lazy" /> : <span key={key}>{raw.alt ?? ""}</span>;
    }
    case "thematicBreak": return <hr key={key} />;
    case "break": return <br key={key} />;
    case "html": return <span key={key} dangerouslySetInnerHTML={{ __html: sanitizeHtml(raw.value, parent.htmlPolicy) }} />;
    case "table": {
      if (parent.components.table) {
        const custom = parent.components.table({ head: (raw.children[0]?.children ?? []).map((cell: any, i: number) => <Fragment key={i}>{renderChildren(cell.children, context)}</Fragment>), rows: raw.children.slice(1).map((row: any) => row.children.map((cell: any, j: number) => <Fragment key={j}>{renderChildren(cell.children, context)}</Fragment>)), align: raw.align ?? [] });
        if (custom !== undefined) return <Fragment key={key}>{custom}</Fragment>;
      }
      return <table key={key} className={cls(parent.classNames.table)}><thead><tr>{raw.children[0]?.children.map((cell: any, i: number) => <th key={i}>{renderChildren(cell.children, context)}</th>)}</tr></thead><tbody>{raw.children.slice(1).map((row: any, i: number) => <tr key={i}>{row.children.map((cell: any, j: number) => <td key={j}>{renderChildren(cell.children, context)}</td>)}</tr>)}</tbody></table>;
    }
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

export function MarkdownRenderer({ source, className, onOpenLink, bibliography = new Map(), parseOptions, components, classNames, onText, linkPolicy, htmlPolicy }: MarkdownRendererProps) {
  const tree = parseToTree(source, parseOptions);
  const headingSlugs = buildHeadingSlugs(tree);
  const footnotes = collectFootnotes(tree);
  const context: RenderContext = { headingSlugs, footnotes, onOpenLink, bibliography, components: { ...(onText ? { text: onText } : {}), ...components }, classNames: classNames ?? {}, linkPolicy: linkPolicy ?? {}, htmlPolicy: htmlPolicy ?? {}, key: "root" };
  const content = renderChildren(tree.children, context);
  if (footnotes.size) content.push(<section key="footnotes" className="rk-doc-footnotes"><h2>Notes</h2><ol>{[...footnotes.entries()].map(([id, footnote]) => <li key={id} id={`fn-${id}`}>{renderChildren(footnote.children, { ...context, key: `fn:${id}` })}</li>)}</ol></section>);
  return <article className={`rk-doc-markdown ${className ?? ""}`}>{content}</article>;
}

export function HtmlRenderer({ html, className, safeMode = true, htmlPolicy }: HtmlRendererProps) {
  const content = safeMode ? sanitizeHtml(html, htmlPolicy) : html;
  return <article className={`rk-doc-html ${className ?? ""}`} dangerouslySetInnerHTML={{ __html: content }} />;
}

export function renderMarkdown(source: string, options?: Omit<MarkdownRendererProps, "source">): ReactNode {
  return <MarkdownRenderer source={source} {...options} />;
}

export function renderHtml(html: string, options?: Omit<HtmlRendererProps, "html">): ReactNode {
  return <HtmlRenderer html={html} {...options} />;
}
