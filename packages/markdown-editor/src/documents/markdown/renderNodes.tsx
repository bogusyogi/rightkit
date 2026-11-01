import {
  createContext,
  memo,
  useContext,
  useEffect,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import type { RootContent, PhrasingContent } from "mdast";
import { nodeText } from "./semanticBlocks";
import { FootnotesContext, slug } from "./footnotes";
import { HeadingSlugContext } from "./headingSlugs";
import type { CslEntry } from "@rightkit/doc-render";
import { formatBibliographyEntry, formatCitation } from "@rightkit/doc-render";
import type { OutlineHeading } from "./Outline";
import { CodeBlock } from "./CodeBlock";
import { MathBlock } from "./Math";
import { MermaidBlock } from "./MermaidBlock";
import { grantRelativeFile, openExternal, readRelativeAsset } from "@rightkit/document-core/state/tauriBridge";
import { normalizeLivePreviewUrl } from "@rightkit/document-core/documents/web/webUrl";
import { shortcutLabel } from "@rightkit/document-core/platform";
import type { MarkdownIndexedFile } from "./references/folderIndex";
import { defaultWikiLinkDisplay, resolveWikiLink } from "./wikiLinkResolver";
import { MarkdownRenderTrackerContext } from "./export/MarkdownRenderTracker";

export const MarkdownLinkContext = createContext<{
  documentPath?: string;
  onOpenPath?: (path: string) => void;
  folderFiles?: readonly MarkdownIndexedFile[];
  sessionFiles?: readonly MarkdownIndexedFile[];
} | null>(null);
type ListItemNode = Extract<RootContent, { type: "listItem" }>;
export const MarkdownInteractionContext = createContext<{
  onTaskToggle?: (item: ListItemNode, checked: boolean) => void;
} | null>(null);
export const TableOfContentsContext = createContext<OutlineHeading[]>([]);
export const BibliographyContext = createContext<Map<string, CslEntry>>(new Map());

/** Source offsets survive insertions before a node, unlike array indices. Synthetic nodes fall back
 * to their local slot because they have no durable source identity. */
export function markdownNodeKey(
  node: { type: string; position?: { start?: { offset?: number }; end?: { offset?: number } } },
  index: number,
): string {
  const start = node.position?.start?.offset;
  const end = node.position?.end?.offset;
  return typeof start === "number" && typeof end === "number"
    ? `${node.type}:${start}:${end}`
    : `${node.type}:synthetic:${index}`;
}

/** Minimal, dependency-free mdast → React renderer for plain (non-semantic) blocks. */
export function renderNodes(nodes: RootContent[]): ReactNode {
  return nodes.map((n, i) => <RenderNode key={markdownNodeKey(n, i)} node={n} />);
}

function children(node: { children?: RootContent[] }): ReactNode {
  return (node.children ?? []).map((c, i) => <RenderNode key={markdownNodeKey(c, i)} node={c} />);
}

function inline(nodes: PhrasingContent[]): ReactNode {
  return nodes.map((c, i) => <RenderNode key={markdownNodeKey(c, i)} node={c as RootContent} />);
}

// Memoized: large documents otherwise re-run createElement for every node on every MarkdownView
// render. React.memo does a shallow compare of the single `node` prop (reference equality) — since
// mdast nodes are stable object identities across renders of unchanged content (and markdownNodeKey
// keys reconciliation the same way), this is a correct skip. Context updates (footnotes/heading
// slugs/link/interaction/TOC) still propagate through memoized children — memo only blocks re-render
// from an unchanged-props parent pass, not from context.
const RenderNode = memo(function RenderNode({ node }: { node: RootContent }): ReactNode {
  const footnotes = useContext(FootnotesContext);
  const headingSlugs = useContext(HeadingSlugContext);
  const markdownLinks = useContext(MarkdownLinkContext);
  const interactions = useContext(MarkdownInteractionContext);
  // remark-math nodes (inlineMath / math) aren't in the base mdast union — handle before the switch.
  const t = node.type as string;
  if (t === "inlineMath" || t === "math") {
    return (
      <MathBlock value={(node as unknown as { value: string }).value} display={t === "math"} />
    );
  }
  // RightKit's parser emits `highlight` nodes for ==text==.
  if (t === "highlight") {
    return (
      <mark className="rounded-sm bg-amber-200/20 px-0.5 text-amber-100">
        {inline((node as unknown as { children: PhrasingContent[] }).children)}
      </mark>
    );
  }
  if (t === "wikiLink") {
    return <WikiLinkNode node={node as unknown as WikiLinkMdastNode} />;
  }
  if (t === "abbr") {
    const abbr = node as unknown as { title: string; children: PhrasingContent[] };
    return <abbr title={abbr.title}>{inline(abbr.children as PhrasingContent[])}</abbr>;
  }
  if (t === "descriptionList") {
    return <DescriptionListNode node={node as unknown as DescriptionListMdastNode} />;
  }
  if (t === "citation") {
    return <CitationNode node={node as unknown as CitationMdastNode} />;
  }
  if (t === "bibliography") {
    return <BibliographyNode />;
  }
  switch (node.type) {
    case "heading": {
      const Tag = `h${node.depth}` as "h1" | "h2" | "h3" | "h4" | "h5" | "h6";
      const size = ["1.5em", "1.25em", "1.125em", "1em", "0.95em", "0.95em"][node.depth - 1];
      const lineHeight = node.depth <= 2 ? 1.2 : 1.3;
      // Prefer the per-document dedup map (matches the TOC); fall back to a stateless slug.
      const id = headingSlugs?.get(node) ?? slug(nodeText(node));
      return (
        <Tag
          id={id}
          className="group/h relative mt-5 mb-2 text-balance font-serif font-semibold tracking-tight"
          style={{ fontSize: size, lineHeight }}
        >
          {inline(node.children)}
          <a
            href={`#${id}`}
            aria-label="Link to this section"
            className="ml-2 text-muted-2 no-underline opacity-0 transition-opacity group-hover/h:opacity-100"
          >
            #
          </a>
        </Tag>
      );
    }
    case "paragraph": {
      const text = nodeText(node).trim().toLowerCase();
      if (text === "[toc]") return <TableOfContentsMarker />;
      return <p className="my-3 text-pretty leading-relaxed">{inline(node.children)}</p>;
    }
    case "footnoteReference": {
      const id = node.identifier;
      const num = footnotes?.get(id)?.num;
      return (
        <sup id={`fnref-${id}`} className="text-[0.7em]">
          <a href={`#fn-${id}`} className="px-0.5 text-copper-hi no-underline">
            [{num ?? "?"}]
          </a>
        </sup>
      );
    }
    case "footnoteDefinition":
      return null; // rendered in the footnotes section at the end of the document
    case "text":
      return node.value;
    case "strong":
      return <strong className="font-semibold">{inline(node.children)}</strong>;
    case "emphasis":
      return <em className="italic">{inline(node.children)}</em>;
    case "delete":
      return <del>{inline(node.children)}</del>;
    case "inlineCode":
      return (
        <code className="rounded bg-panel px-1 py-0.5 font-mono text-[0.9em] text-copper-hi">
          {node.value}
        </code>
      );
    case "break":
      return <br />;
    case "link": {
      const url = node.url;
      const isAnchor = url.startsWith("#");
      const isLocalRelative = isMarkdownLocalRelative(url);
      return (
        <a
          href={url}
          // In-page anchors scroll normally. Other links: Ctrl/Cmd+click hands the href (as written)
          // to the brokered opener (http/https/mailto allowed, rest blocked); a plain click never
          // navigates the webview away. Matches the DOCX editor / Notepad "Ctrl+Click to follow".
          title={isAnchor ? undefined : `${shortcutLabel("mod+click")} to follow link`}
          onClick={
            isAnchor
              ? undefined
              : (e) => {
                  e.preventDefault();
                  if (!(e.ctrlKey || e.metaKey)) return;
                  if (isLocalRelative && markdownLinks?.documentPath && markdownLinks.onOpenPath) {
                    void grantRelativeFile(markdownLinks.documentPath, url)
                      .then((path) => {
                        if (path) markdownLinks.onOpenPath?.(path);
                      })
                      .catch(() => {
                        // A denied relative-file grant leaves the rendered Markdown in place.
                      });
                  } else {
                    const normalized = normalizeLivePreviewUrl(url);
                    if (normalized) {
                      window.dispatchEvent(
                        new CustomEvent("viewright:open-url-tab", { detail: normalized }),
                      );
                    } else {
                      void openExternal(url);
                    }
                  }
                }
          }
          className="cursor-pointer text-copper-hi underline decoration-copper/40 hover:decoration-copper"
        >
          {inline(node.children)}
        </a>
      );
    }
    case "image":
      return <MarkdownImage node={node} />;
    case "thematicBreak":
      return <hr className="my-6 border-line" />;
    case "blockquote":
      return (
        <blockquote className="my-4 rounded-md border border-line bg-panel/40 px-4 py-2 text-muted">
          {children(node)}
        </blockquote>
      );
    case "list":
      if (node.children.some((item) => typeof item.checked === "boolean")) {
        return <ul className="my-3 list-none space-y-1 pl-1">{children(node)}</ul>;
      }
      return node.ordered ? (
        <ol className="my-3 list-decimal space-y-1 pl-6">{children(node)}</ol>
      ) : (
        <ul className="my-3 list-disc space-y-1 pl-6">{children(node)}</ul>
      );
    case "listItem":
      if (typeof node.checked === "boolean") {
        const canToggle = !!interactions?.onTaskToggle;
        return (
          <li className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={node.checked}
              disabled={!canToggle}
              onChange={(event) => interactions?.onTaskToggle?.(node, event.currentTarget.checked)}
              className="mt-1 accent-copper"
              aria-label={node.checked ? "Completed task" : "Incomplete task"}
            />
            <div className="min-w-0 flex-1 [&_p]:my-0">{children(node)}</div>
          </li>
        );
      }
      return <li>{children(node)}</li>;
    case "code": {
      const diagram = mermaidCodeFor(node.lang ?? null, node.value);
      return diagram ? (
        <MermaidBlock code={diagram} documentPath={markdownLinks?.documentPath} />
      ) : (
        <CodeBlock code={node.value} lang={node.lang ?? null} />
      );
    }
    case "table": {
      const align =
        (node as typeof node & { align?: Array<"left" | "right" | "center" | null> }).align ?? [];
      return (
        <div className="vr-md-table my-4 max-w-full overflow-x-auto rounded border border-line">
          <table className="w-full min-w-max border-collapse text-sm">
            <tbody>
              {node.children.map((row, ri) => (
                <tr key={ri} className="border-b border-line last:border-b-0">
                  {row.children.map((cell, ci) => {
                    const Cell = ri === 0 ? "th" : "td";
                    const textAlign = align[ci] ?? "left";
                    return (
                      <Cell
                        key={ci}
                        className={
                          ri === 0 ? "px-3 py-1.5 font-medium text-muted" : "px-3 py-1.5"
                        }
                        style={{ textAlign }}
                      >
                        {inline(cell.children as PhrasingContent[])}
                      </Cell>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    default:
      return null;
  }
});

function MarkdownImage({ node }: { node: Extract<RootContent, { type: "image" }> }) {
  const markdownLinks = useContext(MarkdownLinkContext);
  const tracker = useContext(MarkdownRenderTrackerContext);
  const [src, setSrc] = useState(node.url);

  useEffect(() => {
    let cancelled = false;
    setSrc(node.url);
    if (!isMarkdownLocalRelative(node.url) || !markdownLinks?.documentPath) return;
    const finish = tracker?.begin(`Image: ${node.url}`);
    void readRelativeAsset(markdownLinks.documentPath, node.url)
      .then((asset) => {
        if (!cancelled && asset) setSrc(`data:${asset.mime};base64,${asset.base64}`);
      })
      .catch((error) => tracker?.fail(`Image: ${node.url}`, error))
      .finally(() => finish?.());
    return () => {
      cancelled = true;
      finish?.();
    };
  }, [node.url, markdownLinks?.documentPath, tracker]);

  return (
    <img
      data-vr-export-state={
        src === node.url && isMarkdownLocalRelative(node.url) ? "loading" : "ready"
      }
      src={src}
      alt={node.alt ?? ""}
      title={node.title ?? undefined}
      className="my-4 max-w-full rounded"
    />
  );
}

function TableOfContentsMarker() {
  const headings = useContext(TableOfContentsContext);
  if (headings.length === 0) return null;
  return (
    <nav data-vr-toc-marker className="my-5 rounded-lg border border-line bg-panel p-4">
      <div className="mb-2 font-mono text-chrome-xs uppercase tracking-[0.2em] text-muted-2">
        Table of contents
      </div>
      <ol className="space-y-1">
        {headings.map((heading) => (
          <li
            key={`${heading.id}-${heading.text}`}
            style={{ paddingLeft: (heading.depth - 1) * 12 }}
          >
            <a
              href={`#${heading.id}`}
              className="text-sm text-muted no-underline hover:text-copper-hi"
            >
              {heading.text}
            </a>
          </li>
        ))}
      </ol>
    </nav>
  );
}

function mermaidCodeFor(lang: string | null, code: string): string | null {
  const key = (lang ?? "").toLowerCase();
  if (key === "mermaid" || key === "mmd") return code;
  if (key === "flowchart" || key === "graph")
    return /^\s*(flowchart|graph)\b/i.test(code) ? code : `flowchart TD\n${code}`;
  if (key === "sequence")
    return /^\s*sequenceDiagram\b/i.test(code) ? code : `sequenceDiagram\n${code}`;
  if (key === "classdiagram" || key === "class")
    return /^\s*classDiagram\b/i.test(code) ? code : `classDiagram\n${code}`;
  if (key === "statediagram" || key === "state")
    return /^\s*stateDiagram/i.test(code) ? code : `stateDiagram-v2\n${code}`;
  if (key === "erdiagram" || key === "er")
    return /^\s*erDiagram\b/i.test(code) ? code : `erDiagram\n${code}`;
  if (key === "journey") return /^\s*journey\b/i.test(code) ? code : `journey\n${code}`;
  if (key === "gantt") return /^\s*gantt\b/i.test(code) ? code : `gantt\n${code}`;
  if (key === "pie") return /^\s*pie\b/i.test(code) ? code : `pie\n${code}`;
  if (key === "gitgraph" || key === "git")
    return /^\s*gitGraph\b/i.test(code) ? code : `gitGraph\n${code}`;
  if (key === "mindmap") return /^\s*mindmap\b/i.test(code) ? code : `mindmap\n${code}`;
  if (key === "timeline") return /^\s*timeline\b/i.test(code) ? code : `timeline\n${code}`;
  if (key === "quadrant" || key === "quadrantchart")
    return /^\s*quadrantChart\b/i.test(code) ? code : `quadrantChart\n${code}`;
  if (key === "requirement" || key === "requirementdiagram")
    return /^\s*requirementDiagram\b/i.test(code) ? code : `requirementDiagram\n${code}`;
  if (key === "sankey") return /^\s*sankey-beta\b/i.test(code) ? code : `sankey-beta\n${code}`;
  if (key === "xy" || key === "xychart")
    return /^\s*xychart-beta\b/i.test(code) ? code : `xychart-beta\n${code}`;
  if (key === "block") return /^\s*block-beta\b/i.test(code) ? code : `block-beta\n${code}`;
  if (key === "packet") return /^\s*packet-beta\b/i.test(code) ? code : `packet-beta\n${code}`;
  return null;
}

function isMarkdownLocalRelative(url: string): boolean {
  const u = url.trim();
  return (
    !!u &&
    !/^[a-z][a-z0-9+.-]*:/i.test(u) &&
    !u.startsWith("//") &&
    !u.startsWith("/") &&
    !u.startsWith("#") &&
    !u.startsWith("?")
  );
}

interface WikiLinkMdastNode {
  type: "wikiLink";
  target: string;
  hash: string;
  display: string | null;
}

interface DescriptionListMdastNode {
  type: "descriptionList";
  children: {
    type: "descriptionItem";
    terms: { type: "descriptionTerm"; children: PhrasingContent[] }[];
    details: { type: "descriptionDetails"; children: PhrasingContent[] }[];
  }[];
}

function WikiLinkNode({ node }: { node: WikiLinkMdastNode }) {
  const markdownLinks = useContext(MarkdownLinkContext);
  const { target, hash, display } = node;
  const label = display ?? defaultWikiLinkDisplay(target, hash);
  const resolved = target
    ? resolveWikiLink(target, {
        documentPath: markdownLinks?.documentPath,
        folderFiles: markdownLinks?.folderFiles,
        sessionFiles: markdownLinks?.sessionFiles,
      })
    : null;
  const href = target ? (hash ? `${target}#${hash}` : target) : `#${hash}`;

  return (
    <a
      href={href}
      title={`${shortcutLabel("mod+click")} to follow link`}
      onClick={(e: ReactMouseEvent<HTMLAnchorElement>) => {
        e.preventDefault();
        if (!(e.ctrlKey || e.metaKey)) return;
        if (resolved?.absolutePath && markdownLinks?.onOpenPath) {
          markdownLinks.onOpenPath(resolved.absolutePath);
          return;
        }
        if (markdownLinks?.documentPath && target && isMarkdownLocalRelative(target)) {
          void grantRelativeFile(markdownLinks.documentPath, target)
            .then((path) => {
              if (path) markdownLinks.onOpenPath?.(path);
            })
            .catch(() => {
              // A denied relative-file grant leaves the rendered link in place.
            });
          return;
        }
        const normalized = normalizeLivePreviewUrl(target);
        if (normalized) {
          window.dispatchEvent(new CustomEvent("viewright:open-url-tab", { detail: normalized }));
        } else if (target) {
          void openExternal(target);
        }
      }}
      className="cursor-pointer text-copper-hi underline decoration-copper/40 hover:decoration-copper"
    >
      {label}
    </a>
  );
}

function DescriptionListNode({ node }: { node: DescriptionListMdastNode }) {
  return (
    <dl className="my-4 space-y-2">
      {node.children.map((item, i) => (
        <div key={i} className="grid grid-cols-1 gap-1 sm:grid-cols-[max-content_1fr]">
          {item.terms.map((term, ti) => (
            <dt key={ti} className="font-semibold text-paper">
              {inline(term.children as PhrasingContent[])}
            </dt>
          ))}
          {item.details.map((detail, di) => (
            <dd key={di} className="text-paper/90">
              {inline(detail.children as PhrasingContent[])}
            </dd>
          ))}
        </div>
      ))}
    </dl>
  );
}

interface CitationMdastNode {
  type: "citation";
  key: string;
  locator?: string;
}

function CitationNode({ node }: { node: CitationMdastNode }) {
  const bibliography = useContext(BibliographyContext);
  const entry = bibliography.get(node.key);
  if (!entry) {
    return <span className="text-muted-2">{`[@${node.key}]`}</span>;
  }
  return <span className="text-muted-2">{formatCitation(entry, node.locator)}</span>;
}

function BibliographyNode() {
  const bibliography = useContext(BibliographyContext);
  const entries = Array.from(bibliography.values());
  if (entries.length === 0) {
    return <p className="text-muted-2 italic">[bibliography]</p>;
  }
  return (
    <div className="my-4">
      <ol className="list-decimal space-y-1 pl-6">
        {entries.map((entry) => (
          <li key={entry.id} className="text-paper/90">
            {formatBibliographyEntry(entry)}
          </li>
        ))}
      </ol>
    </div>
  );
}
