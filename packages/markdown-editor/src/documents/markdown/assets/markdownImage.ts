/**
 * Markdown image transactional helper.
 *
 * Implements the async state machine described in docs/plans/2026-08-13-block-local-editing-repair.md M3:
 *   capture bookmark/revision -> Save As if needed -> save asset -> map bookmark -> insert -> commit
 * Any failed transition rolls back the asset.
 *
 * Local assets are stored in `<document-stem>.assets/` beside the document.
 * The asset service (`save_markdown_asset`) already handles 20 MiB, MIME allowlist,
 * sanitized SVG, collision handling (`-2..-999`), and symlink containment.
 *
 * Frontend size validation measures decoded bytes exactly as Rust does
 * (`decodedBase64ByteLength` / `validateBase64AssetSize` in markdownAsset.ts).
 */

import type { EditorView } from "prosemirror-view";
import { bytesToBase64 } from "@rightkit/document-core/documents/base64";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { MarkdownCommitSummary } from "../core/MarkdownDocumentTypes";
import {
  MAX_MARKDOWN_ASSET_BYTES,
  decodeBase64Bytes,
  type MarkdownAssetMimeKind,
  type MarkdownAssetClient,
  NativeMarkdownAssetClient,
  validateBase64AssetSize,
  validateMarkdownAsset,
} from "./markdownAsset";
import { pmSelectionToLogicalRange } from "../projection/pmSelectionBridge";
import { saveFileDialog, regrantSavedTarget, writeDocument } from "@rightkit/document-core/state/tauriBridge";
import { formatFromPath, isUntitled } from "@rightkit/document-core/documents/types";

export type ImageInsertState =
  | "idle"
  | "save-as"
  | "saving-asset"
  | "inserting"
  | "committing"
  | "done"
  | "error";

export interface MarkdownImageInsertOptions {
  readonly core: MarkdownDocumentCore;
  /** Current document path; may be untitled (`untitled:N` or `undefined`). */
  readonly documentPath?: string;
  /** Active EditorView for bookmark capture; optional when inserting from source surface. */
  readonly view?: EditorView | null;
  /** Selection bookmarks as logical source offsets; used when view not available. */
  readonly selection?: { from: number; to: number };
  /** Bookmark captured synchronously at drop time. Its summaries are filled by the owner while
   * asynchronous asset work is in flight, so edits made after the event are not lost. */
  readonly bookmark?: MarkdownLogicalBookmark;
  /** Raw bytes + filename + mime for the local image. For URL images, use `insertImageUrl` instead. */
  readonly file?: { bytes: Uint8Array; name: string; mime: MarkdownAssetMimeKind };
  /** For already-base64 payload (e.g., clipboard bitmap temp file read). */
  readonly base64Payload?: string;
  readonly hintName: string;
  readonly mimeKind: MarkdownAssetMimeKind;
  readonly alt: string;
  readonly title?: string | null;
  /** Optional callback for state transitions (for UI). */
  readonly onState?: (state: ImageInsertState, message?: string) => void;
  /** Access to current edits to write back after Save As; when absent we fall back to core snapshot. */
  readonly getDocumentText?: () => string;
  /** Optional callback after Save As succeeds to update the app's active path. */
  readonly onSaveAsSuccess?: (newPath: string) => Promise<void> | void;
  /** Abort hook owned by native-drop coordination. */
  readonly isCancelled?: () => boolean;
  /** Injectable client for focused tests; production uses the native client. */
  readonly assetClient?: MarkdownAssetClient;
}

export interface MarkdownLogicalBookmark {
  readonly from: number;
  readonly to: number;
  readonly revision: number;
  readonly summaries: MarkdownCommitSummary[];
}

const IMAGE_EXT_TO_MIME: Record<string, MarkdownAssetMimeKind> = {
  png: "png",
  jpg: "jpeg",
  jpeg: "jpeg",
  webp: "webp",
  gif: "gif",
  svg: "svg",
};

export function mimeFromFileName(name: string): MarkdownAssetMimeKind | null {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return IMAGE_EXT_TO_MIME[ext] ?? null;
}

export function altPrefillFromFileName(name: string): string {
  return altFromFileName(name);
}

export function isHttpUrl(url: string): boolean {
  return /^https?:\/\//i.test(url.trim());
}

function escapeImageAlt(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/\[/g, "\\[").replace(/\]/g, "\\]");
}

function escapeImageUrl(value: string): string {
  return value
    .replace(/\\/g, "\\\\")
    .replace(/ /g, "%20")
    .replace(/\(/g, "\\(")
    .replace(/\)/g, "\\)");
}

function escapeImageTitle(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function mapPositionThroughSummaries(
  pos: number,
  summaries: readonly MarkdownCommitSummary[],
  assoc: -1 | 1 = 1,
): number {
  let mapped = Math.max(0, pos);
  for (const summary of summaries) {
    let delta = 0;
    let resolved = false;
    for (const ch of summary.oldToNew) {
      const oldFrom = ch.fromA;
      const oldTo = ch.toA;
      if (pos < oldFrom || (pos === oldFrom && oldFrom !== oldTo && assoc < 0)) break;
      if (pos > oldTo || (pos === oldTo && oldFrom !== oldTo && assoc > 0)) {
        delta += ch.toB - ch.fromB - (oldTo - oldFrom);
        continue;
      }
      // A position inside a replacement maps to its associated edge. For an
      // insertion, assoc chooses before/after the inserted text.
      mapped = (assoc < 0 ? ch.fromB : ch.toB) + delta;
      resolved = true;
      break;
    }
    if (!resolved) mapped = pos + delta;
    pos = mapped;
  }
  return Math.max(0, mapped);
}

function selectionFromView(
  view: EditorView,
  snapshot: { logicalSource: string },
): { from: number; to: number } {
  const mapped = pmSelectionToLogicalRange(view.state.doc, view.state.selection, snapshot);
  return {
    from: Math.min(mapped.anchor, mapped.head),
    to: Math.max(mapped.anchor, mapped.head),
  };
}

async function ensureSavedDocument(options: {
  core: MarkdownDocumentCore;
  documentPath?: string;
  getDocumentText?: () => string;
  onSaveAsSuccess?: (newPath: string) => Promise<void> | void;
  isCancelled?: () => boolean;
}): Promise<string | null> {
  if (options.isCancelled?.()) return null;
  const docPath = options.documentPath;
  if (docPath && !isUntitled(docPath)) return docPath;
  // Untitled or no path -> Save As
  const text = options.getDocumentText?.() ?? options.core.snapshot().logicalSource;
  const format = docPath ? formatFromPath(docPath) : "md";
  const ext = format === "mdx" ? "mdx" : format === "md" ? "md" : "md";
  // Use first-line title heuristic similar to useDocumentSaver.untitledSaveName
  const firstLine =
    text
      .split(/\r?\n/)[0]
      ?.replace(/^#+\s*/, "")
      .trim() ?? "";
  const stem = firstLine
    ? firstLine.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 40) || "untitled"
    : "untitled";
  const defaultName = `${stem}.${ext}`;
  const target = await saveFileDialog(defaultName, ext);
  if (options.isCancelled?.()) return null;
  if (!target) return null;
  try {
    const written = await writeDocument(target, text);
    if (options.isCancelled?.()) return null;
    void written;
    const granted = (await regrantSavedTarget(target)) ?? target;
    if (options.isCancelled?.()) return null;
    await options.onSaveAsSuccess?.(granted);
    if (options.isCancelled?.()) return null;
    return granted;
  } catch {
    return null;
  }
}

/**
 * Insert a local image file transactionally.
 *
 * State machine:
 *   1. capture bookmark + revision
 *   2. ensure document is saved (Save As if needed)
 *   3. validate size (decoded bytes)
 *   4. save asset via Rust (returns rollback token)
 *   5. map bookmark through any intervening edits
 *   6. insert markdown `![alt](relative/path "title")` at mapped position
 *   7. commit token on success, rollback on any failure
 */
export async function insertLocalMarkdownImage(
  options: MarkdownImageInsertOptions,
): Promise<{ ok: boolean; error?: string }> {
  const { core, view, hintName, mimeKind, alt, title } = options;
  let docPath = options.documentPath;
  const client = options.assetClient ?? new NativeMarkdownAssetClient();
  const isCancelled = options.isCancelled ?? (() => false);
  let rollbackToken: string | null = null;
  let committed = false;
  let rolledBack = false;
  const onState = options.onState ?? (() => {});

  // Capture bookmark & revision
  const snapshotAtCapture = core.snapshot();
  const revisionAtCapture = options.bookmark?.revision ?? snapshotAtCapture.revision;
  let bookmarkFrom: number;
  let bookmarkTo: number;
  if (options.bookmark) {
    bookmarkFrom = options.bookmark.from;
    bookmarkTo = options.bookmark.to;
  } else if (view) {
    const selection = selectionFromView(view, snapshotAtCapture);
    bookmarkFrom = selection.from;
    bookmarkTo = selection.to;
  } else if (options.selection) {
    bookmarkFrom = options.selection.from;
    bookmarkTo = options.selection.to;
  } else {
    bookmarkFrom = snapshotAtCapture.logicalSource.length;
    bookmarkTo = snapshotAtCapture.logicalSource.length;
  }

  // Track intervening commits after capture to map bookmark
  const interveningSummaries = options.bookmark?.summaries ?? [];
  const unsub = options.bookmark
    ? () => {}
    : core.subscribe(0, (summary) => {
        // Only record commits that happened after capture
        if (summary.revision > revisionAtCapture) interveningSummaries.push(summary);
      });

  let sourceSummary: MarkdownCommitSummary | null = null;
  let sourceRolledBack = false;

  const throwIfCancelled = () => {
    if (isCancelled()) throw new Error("Image insertion cancelled");
  };

  const rollbackSource = () => {
    if (!sourceSummary || sourceRolledBack) return;
    sourceRolledBack = true;
    const committedSource = sourceSummary;
    const current = core.snapshot();
    const later = interveningSummaries.filter(
      (summary) => summary.revision > committedSource.revision,
    );
    const edits = committedSource.inverseEdits.map((edit) => ({
      from: mapPositionThroughSummaries(edit.from, later, -1),
      to: mapPositionThroughSummaries(edit.to, later, 1),
      insert: edit.insert,
      inheritMode: "preserve" as const,
    }));
    core.applyCommand({
      edits,
      expectedRevision: current.revision,
      origin: "hybrid",
      label: "Delete",
      isolateHistory: true,
    });
  };

  const rollback = async () => {
    if (!rollbackToken || committed || rolledBack) return;
    rolledBack = true;
    try {
      await client.rollback(rollbackToken);
    } catch {
      // Preserve original failure; native rollback is best effort after an IPC fault.
    }
    rollbackToken = null;
  };

  try {
    // 1. Save As if needed
    throwIfCancelled();
    onState("save-as");
    const ensured = await ensureSavedDocument({
      core,
      documentPath: docPath,
      getDocumentText: options.getDocumentText,
      onSaveAsSuccess: options.onSaveAsSuccess,
      isCancelled,
    });
    if (!ensured) throw new Error("Save cancelled");
    docPath = ensured;
    throwIfCancelled();

    // 2. Prepare bytes
    let bytes: Uint8Array | null = null;
    let payloadBase64: string;
    if (options.file) {
      bytes = options.file.bytes;
      payloadBase64 = bytesToBase64(bytes);
    } else if (options.base64Payload) {
      payloadBase64 = options.base64Payload;
      bytes = decodeBase64Bytes(payloadBase64);
      if (!bytes) throw new Error("Image payload is not valid Base64 data.");
    } else {
      throw new Error("No image data");
    }
    throwIfCancelled();

    // 3. Frontend size validation exactly as Rust (decoded bytes)
    onState("saving-asset");
    const sizeErr = bytes
      ? validateMarkdownAsset({ source: bytes, hintName, mimeKind })
      : validateBase64AssetSize(payloadBase64);
    if (sizeErr) throw sizeErr;
    // Also check base64 decoded length exactly
    const b64SizeErr = validateBase64AssetSize(payloadBase64);
    if (b64SizeErr) throw b64SizeErr;
    if (bytes && bytes.byteLength > MAX_MARKDOWN_ASSET_BYTES) {
      throw new Error(`Asset exceeds ${MAX_MARKDOWN_ASSET_BYTES} bytes.`);
    }

    // 4. Save asset (Rust enforces 20MiB, mime, SVG sanitization, collision, containment)
    const saved = await client.save({
      payloadBase64,
      hintName,
      mimeKind,
      documentPath: docPath,
    });
    const savedRelative = saved.relativePath;
    rollbackToken = saved.rollbackToken ?? null;
    if (!savedRelative) throw new Error("Asset save failed");
    throwIfCancelled();

    // 5. Map bookmark through intervening transactions
    const collapsed = bookmarkFrom === bookmarkTo;
    const mappedFrom = mapPositionThroughSummaries(
      bookmarkFrom,
      interveningSummaries,
      collapsed ? 1 : -1,
    );
    const mappedTo = mapPositionThroughSummaries(bookmarkTo, interveningSummaries, 1);
    // Clamp to current source length (snapshot may have advanced)
    const currentSnapshot = core.snapshot();
    const currentLen = currentSnapshot.logicalSource.length;
    const from = Math.max(0, Math.min(mappedFrom, currentLen));
    const to = Math.max(from, Math.min(mappedTo, currentLen));
    throwIfCancelled();

    // 6. Insert markdown reference at mapped position
    onState("inserting");
    const safeAlt = alt ?? altPrefillFromFileName(hintName);
    const titlePart = title ? ` "${escapeImageTitle(title)}"` : "";
    const escapedAlt = escapeImageAlt(safeAlt);
    const escapedUrl = escapeImageUrl(savedRelative);
    // Insert with forward-slash relative path (Rust already returns forward slashes)
    // Ensure we insert as a block if at document start/end or inside empty paragraph? For MVP, insert at offset with newlines handling.
    const insertText = `![${escapedAlt}](${escapedUrl}${titlePart})`;
    const edits = [
      {
        from,
        to,
        insert: insertText,
        inheritMode: "preserve" as const,
      },
    ];

    sourceSummary = core.applyCommand({
      edits,
      expectedRevision: core.snapshot().revision,
      origin: "hybrid",
      label: "Format",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    });
    throwIfCancelled();

    // 7. Commit token. A failed commit is a failed transaction: remove the
    // newly-created file while its rollback authority is still valid.
    onState("committing");
    if (rollbackToken) {
      const token = rollbackToken;
      throwIfCancelled();
      const didCommit = await client.commit(token);
      if (isCancelled()) {
        // The source rollback below prevents a late insertion. Native rollback is best effort if
        // commit crossed the cancellation boundary; test/fallback clients can still remove it.
        await client.rollback(token).catch(() => false);
        throw new Error("Image insertion cancelled");
      }
      if (!didCommit) throw new Error("Asset commit failed");
      committed = true;
      rollbackToken = null;
    }
    onState("done");
    return { ok: true };
  } catch (e) {
    try {
      rollbackSource();
    } catch {
      // Preserve the original failure; source rollback is best effort after a concurrent edit.
    }
    await rollback();
    const msg = e instanceof Error ? e.message : String(e);
    onState("error", msg);
    return { ok: false, error: msg };
  } finally {
    unsub();
  }
}

/**
 * Insert an image by URL (no asset save). Stores URL directly.
 * Validates that URL is http/https only per spec.
 */
export async function insertImageUrl(options: {
  core: MarkdownDocumentCore;
  documentPath?: string;
  view?: EditorView | null;
  selection?: { from: number; to: number };
  url: string;
  alt: string;
  title?: string | null;
}): Promise<{ ok: boolean; error?: string }> {
  const { core, url, alt, title } = options;
  if (!isHttpUrl(url)) {
    return { ok: false, error: "URL must be http:// or https://" };
  }
  const snapshot = core.snapshot();
  let from: number;
  let to: number;
  if (options.selection) {
    from = options.selection.from;
    to = options.selection.to;
  } else if (options.view) {
    const selection = selectionFromView(options.view, snapshot);
    from = selection.from;
    to = selection.to;
  } else {
    from = snapshot.logicalSource.length;
    to = snapshot.logicalSource.length;
  }
  const safeAlt = alt ?? "";
  const escapedAlt = escapeImageAlt(safeAlt);
  const titlePart = title ? ` "${escapeImageTitle(title)}"` : "";
  const insertText = `![${escapedAlt}](${escapeImageUrl(url.trim())}${titlePart})`;
  try {
    core.applyCommand({
      edits: [{ from, to, insert: insertText, inheritMode: "preserve" as const }],
      expectedRevision: core.snapshot().revision,
      origin: "hybrid",
      label: "Format",
      isolateHistory: true,
    });
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export function isImageMime(mime: string): boolean {
  return [
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/webp",
    "image/gif",
    "image/svg+xml",
  ].includes(mime.toLowerCase());
}

export function kindForMime(mime: string): MarkdownAssetMimeKind | null {
  const lower = mime.toLowerCase();
  if (lower === "image/png") return "png";
  if (lower === "image/jpeg" || lower === "image/jpg") return "jpeg";
  if (lower === "image/webp") return "webp";
  if (lower === "image/gif") return "gif";
  if (lower === "image/svg+xml") return "svg";
  return null;
}

export function altFromFileName(fileName: string): string {
  const base = fileName.replace(/\.[^.]+$/, "");
  return base.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
}
