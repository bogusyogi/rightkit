import { useMemo, type MouseEvent as ReactMouseEvent, type RefObject } from "react";
import type { Heading } from "mdast";
import type { MarkdownCoreSnapshot } from "./core/MarkdownDocumentTypes";
import { parseToTree } from "./parseMarkdown";
import { buildBlocks } from "./semanticBlocks";
import { SemanticBlock } from "./components";
import {
  BibliographyContext,
  MarkdownLinkContext,
  TableOfContentsContext,
  renderNodes,
} from "./renderNodes";
import { collectFootnotes, FootnotesContext } from "./footnotes";
import { buildHeadingSlugs, HeadingSlugContext } from "./headingSlugs";
import { nodeText } from "./semanticBlocks";
import type { OutlineHeading } from "./Outline";
import { firstSourceLine } from "./markdownTools";
import { MarkdownSafeHtml, MarkdownSafeHtmlContext } from "./safeHtml/MarkdownSafeHtml";

export interface MarkdownDocumentBodyProps {
  snapshot: MarkdownCoreSnapshot;
  documentPath?: string;
  devMode?: boolean;
  showOutline?: boolean;
  showBackToTop?: boolean;
  interactiveTasks?: boolean;
  safeHtmlEnabled: boolean;
  exportMode?: boolean;
  onOpenPath?: (path: string) => void;
  onSourceJump?: (line: number) => void;
  onPreviewLineChange?: (line: number) => void;
  articleRef?: RefObject<HTMLElement | null>;
  onArticleMouseDown?: (event: ReactMouseEvent<HTMLElement>) => void;
  onArticleClick?: (event: ReactMouseEvent<HTMLElement>) => void;
}

export function MarkdownDocumentBody({
  snapshot,
  documentPath,
  devMode = false,
  exportMode = false,
  safeHtmlEnabled,
  onOpenPath,
  articleRef,
  onArticleMouseDown,
  onArticleClick,
}: MarkdownDocumentBodyProps) {
  const model = useMemo(() => {
    const tree = parseToTree(snapshot.logicalSource);
    const headingSlugs = buildHeadingSlugs(tree);
    const headings: OutlineHeading[] = tree.children
      .filter((node): node is Heading => node.type === "heading")
      .map((heading) => ({
        depth: heading.depth,
        text: nodeText(heading),
        id: headingSlugs.get(heading) ?? "",
      }));
    return {
      blocks: buildBlocks(tree.children),
      footnotes: collectFootnotes(tree),
      headingSlugs,
      headings,
    };
  }, [snapshot]);
  const bibliography = useMemo(() => new Map(), []);

  return (
    <MarkdownSafeHtmlContext.Provider value={{ enabled: safeHtmlEnabled, documentPath }}>
      <FootnotesContext.Provider value={model.footnotes}>
        <HeadingSlugContext.Provider value={model.headingSlugs}>
          <TableOfContentsContext.Provider value={model.headings}>
            <BibliographyContext.Provider value={bibliography}>
              <MarkdownLinkContext.Provider value={{ documentPath, onOpenPath }}>
                <div className="vr-reading-container w-full">
                  <article
                    ref={articleRef}
                    data-vr-markdown-document=""
                    data-vr-export-state="ready"
                    onMouseDown={exportMode ? undefined : onArticleMouseDown}
                    onClick={exportMode ? undefined : onArticleClick}
                    className="reading vr-reading-measure mx-auto box-border w-full py-10"
                  >
                    {model.blocks.map((block, index) => {
                      const sourceLine =
                        block.kind === "plain" ? firstSourceLine(block.nodes) : null;
                      if (block.kind === "plain") {
                        return (
                          <div key={`plain:${sourceLine ?? index}`}>
                            {block.nodes.map((node, nodeIndex) => (
                              <div
                                key={`${node.type}:${node.position?.start.offset ?? nodeIndex}`}
                                data-md-line={node.position?.start.line ?? sourceLine ?? undefined}
                              >
                                {node.type === "html" ? (
                                  <MarkdownSafeHtml raw={node.value} />
                                ) : (
                                  renderNodes([node])
                                )}
                              </div>
                            ))}
                          </div>
                        );
                      }
                      return (
                        <div
                          key={`${block.hit.kind}:${sourceLine ?? index}`}
                          data-md-line={sourceLine ?? undefined}
                          className="relative"
                        >
                          {devMode && !exportMode && (
                            <span className="absolute -left-2 -top-2 z-10 rounded bg-copper/20 px-1.5 py-0.5 font-mono text-chrome-xs text-copper-hi">
                              {block.hit.kind} · {(block.hit.confidence * 100).toFixed(0)}%
                              {block.hit.forced ? " · forced" : ""}
                            </span>
                          )}
                          <SemanticBlock block={block} />
                        </div>
                      );
                    })}
                    {model.footnotes.size > 0 && (
                      <section className="mt-10 border-t border-line pt-4 text-sm text-muted">
                        <div className="mb-2 font-mono text-chrome-xs uppercase tracking-[0.2em] text-muted-2">
                          Notes
                        </div>
                        <ol className="space-y-2">
                          {[...model.footnotes.entries()]
                            .sort((left, right) => left[1].num - right[1].num)
                            .map(([id, footnote]) => (
                              <li key={id} id={`fn-${id}`} className="flex gap-2.5 leading-relaxed">
                                <span className="shrink-0 font-mono text-xs text-muted-2">
                                  {footnote.num}.
                                </span>
                                <div className="min-w-0 flex-1 [&_p]:my-0 [&_p]:inline">
                                  {renderNodes(footnote.children)}
                                  <a
                                    href={`#fnref-${id}`}
                                    className="ml-1.5 text-copper-hi no-underline"
                                    aria-label="Back to reference"
                                  >
                                    ↩
                                  </a>
                                </div>
                              </li>
                            ))}
                        </ol>
                      </section>
                    )}
                  </article>
                </div>
              </MarkdownLinkContext.Provider>
            </BibliographyContext.Provider>
          </TableOfContentsContext.Provider>
        </HeadingSlugContext.Provider>
      </FootnotesContext.Provider>
    </MarkdownSafeHtmlContext.Provider>
  );
}
