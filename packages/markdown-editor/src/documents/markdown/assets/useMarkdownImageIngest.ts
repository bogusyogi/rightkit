/**
 * Image/asset ingest hook for the Markdown hybrid editor.
 *
 * Owns the Paste / Drop / Toolbar pipeline (M3) extracted from
 * MarkdownEditor.tsx: image files from clipboard and DOM drops, HTML
 * smart-paste whose assets are saved through the asset client with a
 * rollback-token lifecycle, and the native OS drop target registered on the
 * editor host. All paths funnel through {@link insertLocalMarkdownImage} so
 * Save-As reconciliation and logical-bookmark selection restore behave the
 * same regardless of ingress.
 */

import { useCallback, useEffect, type MutableRefObject } from "react";
import { TextSelection } from "prosemirror-state";
import type { EditorView } from "prosemirror-view";
import { registerMarkdownDropTarget, type NativeDropPayload } from "@rightkit/document-core/app/dropCoordination";
import { readRelativeAsset } from "@rightkit/document-core/state/tauriBridge";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import { markdownToPmDoc } from "../projection/pmFromMarkdown.js";
import { pmDocToMarkdown } from "../projection/pmToMarkdown.js";
import { assetPayloadBase64, captureLogicalBookmark } from "../projection/pmEditorState.js";
import { commitPmTransactionToCore } from "../projection/pmProjectionSync.js";
import { pmSelectionToLogicalRange } from "../projection/pmSelectionBridge.js";
import {
  decodeBase64Bytes,
  NativeMarkdownAssetClient,
  validateMarkdownAsset,
} from "./markdownAsset.js";
import { DEFAULT_MARKDOWN_ASSET_SETTINGS } from "./markdownAssetSettings.js";
import {
  altPrefillFromFileName,
  insertLocalMarkdownImage,
  isImageMime,
  kindForMime,
  mimeFromFileName,
  type MarkdownLogicalBookmark,
} from "./markdownImage.js";
import { smartPaste } from "./smartPaste.js";

/**
 * Options for {@link useMarkdownImageIngest}.
 */
export interface UseMarkdownImageIngestOptions {
  readonly core: MarkdownDocumentCore | null | undefined;
  readonly documentPath?: string;
  readonly hostRef: MutableRefObject<HTMLDivElement | null>;
  readonly viewRef: MutableRefObject<EditorView | null>;
  readonly onSaveAsSuccessRef: MutableRefObject<
    ((newPath: string) => Promise<void> | void) | undefined
  >;
}

/**
 * Hook return value — the handlers the editor wires into ProseMirror's
 * DOM-event surface plus `pasteThroughCore` for HTML smart-paste.
 * `consumeNativeImageDrop` stays internal to the drop-target effect.
 */
export interface UseMarkdownImageIngestResult {
  readonly pasteThroughCore: (view: EditorView, event: ClipboardEvent) => boolean;
  readonly insertImagesFromFiles: (
    files: File[],
    view: EditorView,
    bookmarkAndRelease?: { bookmark: MarkdownLogicalBookmark; release: () => void },
  ) => Promise<void>;
  readonly handleImagePaste: (view: EditorView, event: ClipboardEvent) => boolean;
  readonly handleImageDrop: (view: EditorView, event: DragEvent) => boolean;
}

export function useMarkdownImageIngest({
  core,
  documentPath,
  hostRef,
  viewRef,
  onSaveAsSuccessRef,
}: UseMarkdownImageIngestOptions): UseMarkdownImageIngestResult {
  const pasteThroughCore = useCallback(
    (view: EditorView, event: ClipboardEvent): boolean => {
      if (!core) return false;
      const html = event.clipboardData?.getData("text/html") ?? "";
      const text = event.clipboardData?.getData("text/plain") ?? "";
      if (!html) return false;
      event.preventDefault();
      const snapshot = core.snapshot();
      const logicalSelection = pmSelectionToLogicalRange(
        view.state.doc,
        view.state.selection,
        snapshot,
      );
      const client = new NativeMarkdownAssetClient();
      const saved: string[] = [];
      void (async () => {
        try {
          const result = await smartPaste({
            html,
            text,
            documentBasename: core.state.documentId.split(/[\\/]/).pop() ?? "document",
            settings: DEFAULT_MARKDOWN_ASSET_SETTINGS,
            insertAt: logicalSelection.head,
            resolveAssetPath: async (payload) => {
              const error = validateMarkdownAsset(payload);
              if (error) throw error;
              const asset = await client.save({
                payloadBase64: assetPayloadBase64(payload.source),
                hintName: payload.hintName,
                mimeKind: payload.mimeKind,
                documentPath: core.state.documentId,
              });
              if (asset.rollbackToken) saved.push(asset.rollbackToken);
              return asset.relativePath;
            },
          });
          const pasted = markdownToPmDoc(result.edits[0]?.insert ?? "");
          const after = view.state.apply(
            view.state.tr.replaceSelection(pasted.slice(0, pasted.content.size)),
          ).doc;
          commitPmTransactionToCore(core, view.state.doc, after, {
            origin: "hybrid",
            label: "Paste",
            isolateHistory: true,
            expectedRevision: snapshot.revision,
            fallbackSource: () => pmDocToMarkdown(after),
          });
          await Promise.all(saved.map((token) => client.commit(token)));
        } catch {
          await Promise.all(saved.map((token) => client.rollback(token)));
        }
      })();
      return true;
    },
    [core],
  );

  // --- Image: Paste / Drop / Toolbar pipeline (M3) ---
  const insertImagesFromFiles = useCallback(
    async (
      files: File[],
      view: EditorView,
      bookmarkAndRelease?: { bookmark: MarkdownLogicalBookmark; release: () => void },
    ) => {
      if (!core || !files.length) return;
      const images = files.filter((f) => isImageMime(f.type) || !!mimeFromFileName(f.name));
      if (!images.length) return;
      const captured = bookmarkAndRelease ?? captureLogicalBookmark(core, view);
      let currentDocumentPath = documentPath;
      const client = new NativeMarkdownAssetClient();
      try {
        for (const file of images) {
          const kind = kindForMime(file.type) ?? mimeFromFileName(file.name);
          if (!kind) continue;
          const bytes = new Uint8Array(await file.arrayBuffer());
          const result = await insertLocalMarkdownImage({
            core,
            documentPath: currentDocumentPath,
            bookmark: captured.bookmark,
            file: { bytes, name: file.name, mime: kind },
            hintName: file.name,
            mimeKind: kind,
            alt: altPrefillFromFileName(file.name),
            assetClient: client,
            getDocumentText: () => core.snapshot().logicalSource,
            onSaveAsSuccess: (newPath) => {
              currentDocumentPath = newPath;
              onSaveAsSuccessRef.current?.(newPath);
            },
          });
          if (!result.ok) throw new Error(result.error ?? "Image insertion failed");
        }
      } catch (e) {
        console.error("[MarkdownEditor] image drop/paste failed:", e);
      } finally {
        captured.release();
      }
    },
    [core, documentPath, onSaveAsSuccessRef],
  );

  const handleImagePaste = useCallback(
    (view: EditorView, event: ClipboardEvent): boolean => {
      if (!core) return false;
      const dt = event.clipboardData;
      if (!dt) return false;
      // 1) Check DataTransfer items for image/* kind=file
      const items = dt.items;
      if (items) {
        const imageFiles: File[] = [];
        for (let i = 0; i < items.length; i++) {
          const it = items[i];
          if (!it) continue;
          if (it.kind === "file" && it.type.startsWith("image/")) {
            const f = it.getAsFile();
            if (f) imageFiles.push(f);
          }
        }
        if (imageFiles.length) {
          event.preventDefault();
          void insertImagesFromFiles(imageFiles, view);
          return true;
        }
      }
      // 2) Check DataTransfer files (copied-file may appear here on some platforms)
      const files = dt.files;
      if (files && files.length) {
        const imgs = Array.from(files).filter(
          (f) => isImageMime(f.type) || !!mimeFromFileName(f.name),
        );
        if (imgs.length) {
          event.preventDefault();
          void insertImagesFromFiles(imgs, view);
          return true;
        }
      }
      // 3) Raw bitmap fallback via Tauri clipboard (async) – only if no html/text paste would otherwise handle
      // If html is empty, try async Tauri readClipboardContent
      const html = dt.getData("text/html");
      const plain = dt.getData("text/plain");
      if (!html && !plain) {
        // Tauri returns broker-granted paths for copied files/bitmaps. Suppress native open-file
        // handling when clipboard carries no text, then insert decoded bytes through normal asset flow.
        event.preventDefault();
        void (async () => {
          try {
            const { readClipboardContent } = await import("@rightkit/document-core/state/tauriBridge");
            const clip = await readClipboardContent();
            const paths =
              clip.kind === "image" && clip.path
                ? [clip.path]
                : clip.kind === "files"
                  ? clip.paths
                  : [];
            if (!paths.length) return;
            const files: File[] = [];
            for (const path of paths) {
              const name = path.split(/[\\/]/).pop() || "clipboard-image";
              const asset = await readRelativeAsset(path, name);
              if (!asset) continue;
              const kind = kindForMime(asset.mime) ?? mimeFromFileName(name);
              const bytes = decodeBase64Bytes(asset.base64);
              if (!kind || !bytes) continue;
              const fileBytes = new Uint8Array(new ArrayBuffer(bytes.byteLength));
              fileBytes.set(bytes);
              files.push(new File([fileBytes.buffer], name, { type: asset.mime }));
            }
            if (files.length) {
              await insertImagesFromFiles(files, view);
            }
          } catch {
            // ignore
          }
        })();
      }
      return false;
    },
    [core, insertImagesFromFiles],
  );

  const handleImageDrop = useCallback(
    (view: EditorView, event: DragEvent): boolean => {
      const dt = (event as unknown as { dataTransfer?: DataTransfer }).dataTransfer;
      if (!dt) return false;
      const files = Array.from(dt.files ?? []);
      const images = files.filter((f) => isImageMime(f.type) || !!mimeFromFileName(f.name));
      if (!images.length) return false;
      if (!core) return false;
      // Non-image drops retain existing app behavior (open-file).
      event.preventDefault();
      event.stopPropagation();
      const coords = { left: (event as MouseEvent).clientX, top: (event as MouseEvent).clientY };
      const pos = view.posAtCoords(coords);
      const captured = captureLogicalBookmark(core, view, {
        x: (event as MouseEvent).clientX,
        y: (event as MouseEvent).clientY,
      });
      if (pos && pos.pos !== undefined) {
        const tr = view.state.tr.setSelection(TextSelection.create(view.state.doc, pos.pos));
        view.dispatch(tr);
      }
      void insertImagesFromFiles(images, view, captured);
      return true;
    },
    [core, insertImagesFromFiles],
  );

  const consumeNativeImageDrop = useCallback(
    async (
      payload: NativeDropPayload,
      context: { isCancelled: () => boolean },
    ): Promise<readonly string[]> => {
      if (!core) return [];
      const view = viewRef.current;
      if (!view) return [];
      const captured = captureLogicalBookmark(core, view, payload.position);
      let currentDocumentPath = documentPath;
      const consumed: string[] = [];
      try {
        for (const path of payload.paths) {
          if (context.isCancelled()) break;
          const name = path.split(/[\\/]/).pop() || "dropped-image";
          const asset = await readRelativeAsset(path, name);
          if (context.isCancelled() || !asset) break;
          const bytes = decodeBase64Bytes(asset.base64);
          const mimeKind = kindForMime(asset.mime) ?? mimeFromFileName(name);
          if (context.isCancelled() || !bytes || !mimeKind) break;
          const result = await insertLocalMarkdownImage({
            core,
            documentPath: currentDocumentPath,
            bookmark: captured.bookmark,
            file: { bytes, name, mime: mimeKind },
            hintName: name,
            mimeKind,
            alt: altPrefillFromFileName(name),
            getDocumentText: () => core.snapshot().logicalSource,
            isCancelled: context.isCancelled,
            onSaveAsSuccess: async (newPath) => {
              currentDocumentPath = newPath;
              await onSaveAsSuccessRef.current?.(newPath);
            },
          });
          if (context.isCancelled() || !result.ok) break;
          consumed.push(path);
        }
      } finally {
        captured.release();
      }
      return consumed;
    },
    [core, documentPath, onSaveAsSuccessRef, viewRef],
  );

  useEffect(() => {
    const host = hostRef.current;
    if (!host || !core) return;
    return registerMarkdownDropTarget(host, consumeNativeImageDrop);
  }, [core, consumeNativeImageDrop, hostRef]);

  return { pasteThroughCore, insertImagesFromFiles, handleImagePaste, handleImageDrop };
}
