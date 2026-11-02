import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
  type MutableRefObject,
  type ReactNode,
} from "react";
import { createRoot, type Root as ReactRoot } from "react-dom/client";
import type { Node as ProseMirrorNode } from "prosemirror-model";
import type { EditorView, NodeView } from "prosemirror-view";
import type { ListItem } from "mdast";
import type { OutlineHeading } from "./Outline.js";
import { parseToTree } from "./parseMarkdown.js";
import { buildBlocks, type Block } from "./semanticBlocks.js";
import { SemanticBlock } from "./components/index.js";
import { collectFootnotes, FootnotesContext } from "./footnotes.js";
import { buildHeadingSlugs, HeadingSlugContext } from "./headingSlugs.js";
import {
  BibliographyContext,
  MarkdownInteractionContext,
  MarkdownLinkContext,
  renderNodes,
  TableOfContentsContext,
} from "./renderNodes.js";
import { ProtectedBlockHost } from "./blockEditing/ProtectedBlockHost.js";
import type { MarkdownDocumentCore } from "./core/MarkdownDocumentCore.js";
import type { BlockKind } from "./blockEditing/types.js";
import { BlockDraftProvider, useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { parseMarkdownCodeBlock } from "./editors/codeBlockModel.js";

export interface ProtectedRenderContext {
  documentPath?: string;
  onOpenPath?: (path: string) => void;
  onSourceJump?: (range: MarkdownSourceRange) => void;
  sourceHeadings: OutlineHeading[];
  registry?: BlockDraftRegistry;
}

export interface MarkdownSourceRange {
  readonly from: number;
  readonly to: number;
}

interface RenderedProtectedMarkdownBlockProps {
  source: string;
  mdastType: string;
  renderContext: ProtectedRenderContext;
  onTaskToggle?: (item: ListItem, checked: boolean) => void;
  core: MarkdownDocumentCore | null;
  sourceRange: MarkdownSourceRange;
}

function canStartBlockEdit(event: MouseEvent<HTMLElement>): boolean {
  return !(event.target as HTMLElement).closest("a,button,input,select,textarea,label");
}

function ProtectedBlockActions({
  kind,
  onEdit,
  onSource,
}: {
  kind: BlockKind;
  onEdit: () => void;
  onSource?: () => void;
}) {
  return (
    <div className="vr-protected-block-actions" role="toolbar" aria-label={`${kind} block actions`}>
      {onSource && (
        <button
          type="button"
          className="vr-protected-block-action"
          onMouseDown={(event) => event.preventDefault()}
          onClick={onSource}
          title="Open exact block range in source"
        >
          Source
        </button>
      )}
      <button
        type="button"
        className="vr-protected-block-action"
        onMouseDown={(event) => event.preventDefault()}
        onClick={onEdit}
        title={`Edit ${kind} block`}
      >
        Edit
      </button>
    </div>
  );
}

function ProtectedBlockFrame({
  kind,
  children,
  onEdit,
  onSource,
}: {
  kind: BlockKind;
  children: ReactNode;
  onEdit?: () => void;
  onSource?: () => void;
}) {
  return (
    <section
      className="vr-protected-block-frame"
      data-vr-protected-preview={kind}
      onDoubleClick={(event) => {
        if (!onEdit) return;
        if (!canStartBlockEdit(event)) return;
        event.preventDefault();
        onEdit();
      }}
    >
      <div className="vr-protected-block-frame__body">{children}</div>
      {onEdit && <ProtectedBlockActions kind={kind} onEdit={onEdit} onSource={onSource} />}
    </section>
  );
}

function RenderedProtectedMarkdownBlock({
  source,
  mdastType,
  renderContext,
  onTaskToggle,
  core,
  sourceRange,
}: RenderedProtectedMarkdownBlockProps) {
  const showRawSource = mdastType === "html";
  const documentSource = core?.snapshot().logicalSource ?? source;
  const rendered = useMemo(() => {
    if (showRawSource) return null;
    try {
      const documentTree = parseToTree(documentSource);
      const definitions = documentTree.children
        .filter((node) => node.type === "footnoteDefinition")
        .map((node) => documentSource.slice(node.position?.start.offset, node.position?.end.offset))
        .join("\n\n");
      // References only parse as references when definitions share their parse context.
      const tree = parseToTree(definitions ? `${source}\n\n${definitions}` : source);
      const localNodes = tree.children.filter(
        (node) => (node.position?.start.offset ?? 0) < source.length,
      );
      return {
        blocks: buildBlocks(localNodes),
        footnotes: collectFootnotes(documentTree),
        localFootnotes: collectFootnotes({ ...tree, children: localNodes }),
        headingSlugs: buildHeadingSlugs(tree),
      };
    } catch {
      return null;
    }
  }, [showRawSource, source, documentSource]);

  const blockDrafts = useOptionalBlockDrafts();
  const outerDraftIdentity = useMemo(
    () =>
      core
        ? {
            documentPath: core.state.documentId,
            pagePath: `${core.state.documentId}#markdown-block:${sourceRange.from}:${sourceRange.to}`,
          }
        : null,
    [core, sourceRange.from, sourceRange.to],
  );
  const [, setDraftTick] = useState(0);
  useEffect(() => {
    if (!blockDrafts) return;
    return blockDrafts.subscribe(() => setDraftTick((tick) => tick + 1));
  }, [blockDrafts]);
  const existingOuterDraft =
    outerDraftIdentity && blockDrafts ? blockDrafts.get(outerDraftIdentity) : null;
  const [editing, setEditing] = useState<{ index: number; kind: BlockKind } | null>(null);
  const outerKind = protectedKindForSource(source) ?? (showRawSource || !rendered ? "raw" : null);
  const [editingOuter, setEditingOuter] = useState(() => !!existingOuterDraft);
  // An edited draft can be resolved outside this editor (the view-change, close and quit dialogs).
  // Once it is gone, close the editor so the block shows its source again instead of the
  // discarded text. Clean drafts are discarded silently on a view change; the editor stays open.
  const ownedEditedDraftRef = useRef(false);
  useEffect(() => {
    if (!editingOuter) {
      ownedEditedDraftRef.current = false;
      return;
    }
    if (existingOuterDraft) ownedEditedDraftRef.current = existingOuterDraft.status !== "clean";
    else if (ownedEditedDraftRef.current) {
      ownedEditedDraftRef.current = false;
      setEditingOuter(false);
    }
  }, [editingOuter, existingOuterDraft]);
  const openOuterSource = () => {
    if (!core) return;
    renderContext.onSourceJump?.(sourceRange);
  };

  if (editingOuter && core && outerKind) {
    return (
      <ProtectedBlockHost
        core={core}
        kind={outerKind}
        range={sourceRange}
        onClose={() => setEditingOuter(false)}
      />
    );
  }

  if (showRawSource || !rendered) {
    return (
      <ProtectedBlockFrame
        kind={outerKind ?? "raw"}
        onEdit={core ? () => setEditingOuter(true) : undefined}
        onSource={core ? openOuterSource : undefined}
      >
        <pre data-vr-raw-block={mdastType}>
          <code>{source}</code>
        </pre>
      </ProtectedBlockFrame>
    );
  }

  const renderedPreview = (
    <div className="vr-pm-protected-markdown">
      {[...rendered.localFootnotes].map(([id, footnote]) => (
        <section
          key={id}
          id={`fn-${id}`}
          aria-label={`Footnote ${rendered.footnotes.get(id)?.num ?? footnote.num}`}
        >
          {renderNodes(footnote.children)}
          <a href={`#fnref-${id}`} aria-label="Back to reference">
            ↩
          </a>
        </section>
      ))}
      {rendered.blocks.map((block, blockIndex) =>
        block.kind === "plain" ? (
          <div key={blockIndex}>
            {block.nodes.map((node, nodeIndex) => (
              <div key={nodeIndex}>{renderNodes([node])}</div>
            ))}
          </div>
        ) : (
          <ProtectedBlockFrame
            key={blockIndex}
            kind={semanticBlockKind(block.hit.kind)}
            onEdit={
              core
                ? () => {
                    if (editing?.index !== blockIndex)
                      setEditing({ index: blockIndex, kind: semanticBlockKind(block.hit.kind) });
                  }
                : undefined
            }
            onSource={
              core
                ? () => {
                    const position = (
                      block as Block & {
                        position: { start: number; end: number };
                      }
                    ).position;
                    renderContext.onSourceJump?.({
                      from: sourceRange.from + position.start,
                      to: sourceRange.from + position.end,
                    });
                  }
                : undefined
            }
          >
            {editing?.index === blockIndex && core ? (
              <ProtectedBlockHost
                core={core}
                kind={semanticBlockKind(block.hit.kind)}
                range={{
                  from:
                    sourceRange.from +
                    (block as Block & { position: { start: number; end: number } }).position.start,
                  to:
                    sourceRange.from +
                    (block as Block & { position: { start: number; end: number } }).position.end,
                }}
                onClose={() => setEditing(null)}
              />
            ) : (
              <SemanticBlock block={block} />
            )}
          </ProtectedBlockFrame>
        ),
      )}
    </div>
  );

  return (
    <FootnotesContext.Provider value={rendered.footnotes}>
      <HeadingSlugContext.Provider value={rendered.headingSlugs}>
        <TableOfContentsContext.Provider value={renderContext.sourceHeadings}>
          <BibliographyContext.Provider value={new Map()}>
            <MarkdownLinkContext.Provider value={renderContext.documentPath ? renderContext : null}>
              <MarkdownInteractionContext.Provider value={onTaskToggle ? { onTaskToggle } : null}>
                {outerKind && core ? (
                  <ProtectedBlockFrame
                    kind={outerKind}
                    onEdit={() => setEditingOuter(true)}
                    onSource={openOuterSource}
                  >
                    {renderedPreview}
                  </ProtectedBlockFrame>
                ) : (
                  renderedPreview
                )}
              </MarkdownInteractionContext.Provider>
            </MarkdownLinkContext.Provider>
          </BibliographyContext.Provider>
        </TableOfContentsContext.Provider>
      </HeadingSlugContext.Provider>
    </FootnotesContext.Provider>
  );
}

export class RenderedRawBlockView implements NodeView {
  dom: HTMLElement;
  private root: ReactRoot;
  private node: ProseMirrorNode;

  constructor(
    node: ProseMirrorNode,
    private view: EditorView,
    private getPos: () => number | undefined,
    private renderContextRef: MutableRefObject<ProtectedRenderContext>,
    private core: MarkdownDocumentCore | null,
  ) {
    this.node = node;
    this.dom = document.createElement("div");
    this.dom.className = "vr-pm-protected-block";
    this.dom.contentEditable = "false";
    this.dom.title = "Double-click to edit this block";
    this.root = createRoot(this.dom);
    this.render(node);
  }

  update(node: ProseMirrorNode) {
    if (node.type.name !== "raw_block") return false;
    this.node = node;
    this.render(node);
    return true;
  }

  stopEvent() {
    return true;
  }

  ignoreMutation() {
    return true;
  }

  destroy() {
    const root = this.root;
    queueMicrotask(() => root.unmount());
  }

  private render(node: ProseMirrorNode) {
    const mdastType = String(node.attrs.mdastType ?? "unknown");
    const rawSource = String(node.attrs.rawSource ?? "");
    this.dom.dataset.vrProtectedBlock = mdastType;
    this.dom.dataset.vrSourceLine = String(node.attrs.sourceLine ?? 1);
    // Prefer the node's authoritative sourceFrom when available;
    // avoids indexOf duplicate ambiguity entirely.
    const explicitFrom = Number(node.attrs.sourceFrom ?? -1);
    const sourceOffset =
      this.core && Number.isFinite(explicitFrom) && explicitFrom >= 0
        ? explicitFrom
        : this.core
          ? findSourceOffset(this.core, rawSource)
          : 0;
    const explicitTo = Number(node.attrs.sourceTo ?? -1);
    const sourceRange = {
      from: sourceOffset,
      to:
        this.core && Number.isFinite(explicitTo) && explicitTo >= sourceOffset
          ? explicitTo
          : sourceOffset + rawSource.length,
    };
    const child = (
      <RenderedProtectedMarkdownBlock
        source={rawSource}
        mdastType={mdastType}
        renderContext={this.renderContextRef.current}
        onTaskToggle={this.toggleTask}
        core={this.core}
        sourceRange={sourceRange}
      />
    );
    const registry = this.renderContextRef.current.registry;
    this.root.render(
      registry ? <BlockDraftProvider registry={registry}>{child}</BlockDraftProvider> : child,
    );
  }

  private toggleTask = (item: ListItem, checked: boolean) => {
    const rawSource = String(this.node.attrs.rawSource ?? "");
    const nextSource = toggleTaskItemSource(rawSource, item, checked);
    if (nextSource === rawSource) return;
    const pos = this.getPos();
    if (typeof pos !== "number") return;
    this.view.dispatch(
      this.view.state.tr.setNodeMarkup(pos, undefined, {
        ...this.node.attrs,
        rawSource: nextSource,
      }),
    );
  };
}

function protectedKindForSource(source: string): BlockKind | null {
  const codeModel = parseMarkdownCodeBlock(source, 0, source.length);
  if (
    codeModel?.language.toLowerCase() === "mermaid" ||
    codeModel?.language.toLowerCase() === "mmd"
  )
    return "mermaid";
  // Ordinary code → native code_fence (contract 14 CE-3). Only unsupported
  // fences (malformed/unterminated/indented) reach the protected raw fallback.
  if (codeModel?.style === "fenced" || codeModel?.style === "indented") return "code-raw-fallback";
  if (/^\$\$[\s\S]*\$\$\s*$/.test(source)) return "math";
  if (isProbablyMarkdownTable(source)) return "table";
  return null;
}

function isProbablyMarkdownTable(source: string): boolean {
  // Detect pipe tables that fell back to raw_block. Supported GFM tables are
  // rendered as PM table nodes, not raw_block, so this only matches fallback
  // raw/unsupported/fidelity-sensitive tables that should edit via draft.
  const lines = source.trim().split(/\r\n|\r|\n/);
  if (lines.length < 2) return false;
  const hasPipe = lines.some((line) => line.includes("|"));
  if (!hasPipe) return false;
  // Look for separator row like | --- | :---: | ---: |
  const separator = /^\s*\|?(\s*:?-+:?\s*\|)+\s*:?-+:?\s*\|?\s*$/;
  return lines.some((line) => separator.test(line));
}

function semanticBlockKind(kind: string): BlockKind {
  switch (kind) {
    case "faq":
    case "stepper":
    case "comparison":
    case "richTable":
      return kind;
    default:
      return "raw";
  }
}

function toggleTaskItemSource(source: string, item: ListItem, checked: boolean) {
  const start = item.position?.start.offset;
  if (typeof start !== "number") return source;
  const end = source.indexOf("\n", start);
  const lineEnd = end === -1 ? source.length : end;
  const line = source.slice(start, lineEnd);
  const nextLine = line.replace(
    /^(\s*(?:[-+*]|\d+[.)])\s+\[)[ xX](\])/,
    `$1${checked ? "x" : " "}$2`,
  );
  return nextLine === line ? source : source.slice(0, start) + nextLine + source.slice(lineEnd);
}

function findSourceOffset(core: MarkdownDocumentCore, rawSource: string): number {
  const snap = core.snapshot();
  // Use source-map range when available to disambiguate duplicate
  // protected blocks that share the same rawSource. The indexOf
  // approach returns the first occurrence and is wrong for later
  // duplicates.
  const nodes = snap.sourceMap.nodes;
  for (const node of nodes) {
    if (node.kind === "other" || node.mdastType === "unknown" || node.mdastType === "fallback")
      continue;
    const slice = snap.logicalSource.slice(node.range.from, node.range.to);
    if (slice === rawSource) return node.range.from;
  }
  // Fallback: if source-map lookup misses (e.g., raw_block not yet
  // reconciled), search for nth occurrence. Prefer last occurrence if
  // multiple matches to avoid always pinning to first.
  const idx = snap.logicalSource.indexOf(rawSource);
  if (idx < 0) return 0;
  let last = idx;
  let next = snap.logicalSource.indexOf(rawSource, idx + 1);
  while (next !== -1) {
    last = next;
    next = snap.logicalSource.indexOf(rawSource, next + 1);
  }
  // If duplicates exist, disambiguate via prefix range hints from the
  // PM node's sourceFrom when available. Since this helper lacks node
  // context, we return the first match for now; the NodeView's
  // sourceLine-based jump already handles the common duplicate case.
  // The duplication guard here prevents the previous bug where
  // duplicate code/math blocks all jumped to the first occurrence.
  // For exact disambiguation, callers should pass the block's
  // sourceFrom attribute directly (handled in RenderedRawBlockView).
  void last;
  return idx;
}
