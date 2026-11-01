import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { buildMarkdownLinkEdit } from "./markdownLink.js";
import {
  FolderIndexCache,
  NativeFolderIndexClient,
  type FolderIndexClient,
  type MarkdownIndexedFile,
} from "./folderIndex.js";
import { MarkdownLinkPopover, type MarkdownLinkChoice } from "./MarkdownLinkPopover.js";
import {
  applyMarkdownLinkCommand,
  headingFragment,
  relativeMarkdownLinkPath,
  resolveMarkdownLinkModel,
  type MarkdownLinkSelectionInput,
  type MarkdownProtectedRange,
} from "./markdownLink.js";

export interface MarkdownLinkPanelProps {
  readonly core: MarkdownDocumentCore;
  readonly documentPath?: string;
  readonly getInsertAt: () => number;
  readonly client?: FolderIndexClient;
  readonly selection?: MarkdownLinkSelectionInput;
  readonly protectedRanges?: readonly MarkdownProtectedRange[];
  readonly onOpenSource?: () => void;
  readonly onClose?: () => void;
}

/** URL-first link controls; local files and headings are secondary pickers. */
export function MarkdownLinkPanel({
  core,
  documentPath,
  getInsertAt,
  client,
  selection: suppliedSelection,
  protectedRanges,
  onOpenSource,
  onClose,
}: MarkdownLinkPanelProps): ReactNode {
  const cache = useRef<FolderIndexCache | null>(null);
  if (!cache.current) cache.current = new FolderIndexCache(client ?? new NativeFolderIndexClient());
  const [files, setFiles] = useState<readonly MarkdownIndexedFile[]>([]);
  const [folderError, setFolderError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const blockDrafts = useOptionalBlockDrafts();
  const [draftTick, setDraftTick] = useState(0);
  useEffect(() => {
    if (!blockDrafts) return;
    return blockDrafts.subscribe(() => setDraftTick((t) => t + 1));
  }, [blockDrafts]);
  void draftTick;
  // biome-ignore lint/correctness/useExhaustiveDependencies: draftTick forces invalidation on registry changes
  const activeDraft = useMemo(() => {
    if (!blockDrafts) return null;
    const docId = documentPath ?? core.state.documentId;
    return blockDrafts.get({ documentPath: docId, pagePath: docId });
  }, [blockDrafts, documentPath, core, draftTick]);
  const getDraftIdentity = () => {
    const docId = documentPath ?? core.state.documentId;
    return { documentPath: docId, pagePath: docId } as const;
  };
  const snapshot = core.snapshot();
  const rawSelection = suppliedSelection ?? { anchor: getInsertAt(), head: getInsertAt() };
  const getDraftDisabledReason = (): string | null => {
    if (!activeDraft) return null;
    if (activeDraft.status === "stale")
      return activeDraft.reason ?? "Block changed outside this editor.";
    if (activeDraft.status === "committing") return "Committing…";
    const base = activeDraft.sourceRange?.start ?? 0;
    const end = activeDraft.sourceRange?.end ?? base + activeDraft.originalText.length;
    const from = Math.min(rawSelection.anchor, rawSelection.head);
    const to = Math.max(rawSelection.anchor, rawSelection.head);
    if (from < base || to > end) return "No valid target in edited block";
    return null;
  };
  const draftDisabledReason = getDraftDisabledReason();
  // When a protected draft is active, the link model must be resolved against the
  // draft's current text at local offsets, preserving the bookmark. The selection
  // is mapped from logical source offsets to draft-local offsets.
  const { model } = useMemo(() => {
    if (activeDraft) {
      const base = activeDraft.sourceRange?.start ?? 0;
      const localAnchor = Math.max(
        0,
        Math.min(rawSelection.anchor - base, activeDraft.currentText.length),
      );
      const localHead = Math.max(
        0,
        Math.min(rawSelection.head - base, activeDraft.currentText.length),
      );
      const localSelection = { anchor: localAnchor, head: localHead };
      const localModel = resolveMarkdownLinkModel({
        source: activeDraft.currentText,
        selection: localSelection,
        links: [],
        protectedRanges: draftDisabledReason
          ? [{ from: 0, to: activeDraft.currentText.length, reason: draftDisabledReason }]
          : protectedRanges,
      });
      // If the draft itself is stale/committing/out-of-range, surface the disabled
      // reason as a protected model so the popover shows the reason instead of silently
      // operating on the canonical source.
      if (draftDisabledReason && localModel.mode !== "protected") {
        const protectedModel = {
          ...localModel,
          mode: "protected" as const,
          protectedReason: draftDisabledReason,
        };
        return { selection: localSelection, model: protectedModel };
      }
      return { model: localModel };
    }
    const sel = rawSelection;
    const m = resolveMarkdownLinkModel({
      source: snapshot.logicalSource,
      selection: sel,
      links: snapshot.sourceMap.nodes.filter((node) => node.kind === "link"),
      protectedRanges,
    });
    return { model: m };
  }, [
    activeDraft,
    draftDisabledReason,
    protectedRanges,
    rawSelection,
    snapshot.logicalSource,
    snapshot.sourceMap.nodes,
  ]);
  const headings = useMemo<readonly MarkdownLinkChoice[]>(
    () =>
      snapshot.sourceMap.nodes.flatMap((node) => {
        const fragment = headingFragment(node.headingId);
        return fragment
          ? [
              {
                key: node.id,
                label: node.semanticKey.split("\0").slice(1).join("\0") || node.semanticKey,
                value: fragment,
                detail: fragment,
              },
            ]
          : [];
      }),
    [snapshot.sourceMap.nodes],
  );
  const localFiles = useMemo<readonly MarkdownLinkChoice[]>(
    () =>
      files.flatMap((file) => {
        const value = relativeMarkdownLinkPath(documentPath, file.absolutePath, file.relativePath);
        return value
          ? [
              {
                key: file.absolutePath,
                label: file.title ?? file.relativePath,
                value,
                detail: value,
              },
            ]
          : [];
      }),
    [documentPath, files],
  );

  const indexFolder = async () => {
    const folderCache = cache.current;
    if (!documentPath || !folderCache || loading) return;
    setLoading(true);
    setFolderError(null);
    try {
      setFiles(await folderCache.fetch(documentPath, true, true));
    } catch (cause) {
      setFolderError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-2" data-testid="markdown-link-panel">
      <MarkdownLinkPopover
        model={model}
        localFiles={localFiles}
        headings={headings}
        localFileDisabledReason={
          documentPath ? undefined : "Choose local files after saving this document to a folder."
        }
        onRequestLocalFiles={() => void indexFolder()}
        onApply={(draft) => {
          if (activeDraft && blockDrafts) {
            const result = buildMarkdownLinkEdit(model, draft);
            if (!result.ok) return result.error;
            const current = activeDraft.currentText;
            const next =
              current.slice(0, result.edit.from) +
              result.edit.insert +
              current.slice(result.edit.to);
            blockDrafts.update(getDraftIdentity(), next, activeDraft.selectionBookmark);
            onClose?.();
            return null;
          }
          const result = applyMarkdownLinkCommand(core, model, draft);
          if (!result.ok) return result.error;
          onClose?.();
          return null;
        }}
        onRemove={
          model.mode === "edit"
            ? () => {
                if (activeDraft && blockDrafts) {
                  const result = buildMarkdownLinkEdit(
                    model,
                    { text: "", address: "", title: "" },
                    true,
                  );
                  if (!result.ok) return result.error;
                  const current = activeDraft.currentText;
                  const next =
                    current.slice(0, result.edit.from) +
                    result.edit.insert +
                    current.slice(result.edit.to);
                  blockDrafts.update(getDraftIdentity(), next, activeDraft.selectionBookmark);
                  onClose?.();
                  return null;
                }
                const result = applyMarkdownLinkCommand(
                  core,
                  model,
                  { text: "", address: "", title: "" },
                  true,
                );
                if (!result.ok) return result.error;
                onClose?.();
                return null;
              }
            : undefined
        }
        onOpenSource={onOpenSource}
        onCancel={onClose}
      />
      {loading && <p className="px-3 text-chrome-xs text-muted-2">Indexing local files…</p>}
      {folderError && (
        <p role="alert" className="px-3 text-chrome-xs text-red-300">
          {folderError}
        </p>
      )}
    </div>
  );
}
