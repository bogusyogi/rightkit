import { useCallback, useRef, useState } from "react";
import type React from "react";
import type { Attachment } from "@rightkit/agent-ui-core/lib/daemon";

export const HERMES_PATHS_MIME = "application/x-hermes-paths";
export const MAX_STAGED_ATTACHMENTS = 8;
export const MAX_STAGED_ATTACHMENT_BYTES = 20 * 1024 * 1024;

/**
 * OpenCovibe's position-anchored paste-chip helpers. A block can stay out of
 * the visible prompt while its user-facing token remains at the cursor; send
 * integration expands the token in place and can append orphaned blocks.
 */
const PASTE_TOKEN_RE = /\[[^[\]]*#(\d+) · [^[\]]*\]/g;

export function buildPasteToken(label: string): string {
  return `[${label}]`;
}

export function singlePasteTokenRe(seq: number): RegExp {
  return new RegExp(`\\[[^[\\]]*#${seq} · [^[\\]]*\\]`);
}

export function insertAtSelection(text: string, selStart: number, selEnd: number, insert: string): string {
  const suffix = text.slice(selEnd);
  const separator = insert.startsWith("[") && suffix.length > 0 && !/^\s/.test(suffix) ? " " : "";
  return text.slice(0, selStart) + insert + separator + suffix;
}

export function parsePasteTokenSeqs(body: string): number[] {
  const seqs: number[] = [];
  const re = new RegExp(PASTE_TOKEN_RE.source, "g");
  let match = re.exec(body);
  while (match !== null) {
    seqs.push(Number(match[1]));
    match = re.exec(body);
  }
  return seqs;
}

export function expandPasteTokens(
  body: string,
  blocks: ReadonlyArray<{ seq?: number; text: string }>,
): { text: string; usedSeqs: Set<number> } {
  const usedSeqs = new Set<number>();
  const re = new RegExp(PASTE_TOKEN_RE.source, "g");
  const text = body.replace(re, (_match, sequence: string) => {
    const seq = Number(sequence);
    const block = blocks.find((candidate) => candidate.seq === seq);
    if (!block) return "";
    usedSeqs.add(seq);
    return block.text;
  });
  return { text, usedSeqs };
}

export interface DroppedFile {
  /** Browser-native File handle. Absent for in-app drags. */
  file?: File;
  /** Absolute filesystem path, or an in-app workspace-relative path. */
  path: string;
  isDirectory?: boolean;
  line?: number;
  lineEnd?: number;
}

export type AttachmentStageKind = "file" | "image" | "audio" | "video" | "folder";
export type AttachmentStageState = "ready" | "uploading" | "error" | "lost";

/** Renderer-only attachment metadata. Bytes are deliberately not persisted. */
export interface StagedAttachment {
  id: string;
  name: string;
  size: number;
  mimeType: string;
  kind: AttachmentStageKind;
  state: AttachmentStageState;
  data?: string;
  path?: string;
  error?: string;
  origin: "local" | "remote" | "in-app";
  previewUrl?: string;
}

export interface UseAttachmentStagingOptions {
  workspaceRoot?: string;
  maxAttachments?: number;
  onInsertInlineRefs?: (refs: string[]) => boolean | void;
  /** Integration-owned upload/stage call. Return true when it accepted the drop. */
  onAttachDroppedItems?: (items: DroppedFile[]) => Promise<boolean> | boolean;
  onStatus?: (message: string) => void;
  requestMainFocus?: () => void;
}

export interface AttachmentStagingProjection {
  attachments: StagedAttachment[];
  stagedAttachments: StagedAttachment[];
  dragActive: boolean;
  addFiles: (files: File[]) => Promise<StagedAttachment[]>;
  attachDroppedItems: (items: DroppedFile[]) => Promise<boolean>;
  removeAttachment: (id: string) => void;
  clearAttachments: () => void;
  restoreAttachments: (items: Array<Partial<StagedAttachment> & Pick<StagedAttachment, "id" | "name" | "mimeType">>) => void;
  handlePaste: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  handleDragEnter: (event: React.DragEvent<HTMLElement>) => void;
  handleDragLeave: (event: React.DragEvent<HTMLElement>) => void;
  handleDragOver: (event: React.DragEvent<HTMLElement>) => void;
  handleDrop: (event: React.DragEvent<HTMLElement>) => void;
  handleInputDragOver: (event: React.DragEvent<HTMLElement>) => void;
  handleInputDrop: (event: React.DragEvent<HTMLElement>) => void;
}

const ACCEPTED_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "audio/mpeg",
  "audio/wav",
  "audio/ogg",
  "audio/webm",
  "audio/mp3",
  "audio/flac",
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
  "video/x-msvideo",
  "text/plain",
  "application/pdf",
  "application/json",
  "text/markdown",
  "text/csv",
]);

function accepts(mimeType: string): boolean {
  return ACCEPTED_TYPES.has(mimeType) || /^(image|audio|video)\//.test(mimeType);
}

function kindFor(mimeType: string): AttachmentStageKind {
  if (mimeType.startsWith("image/")) return "image";
  if (mimeType.startsWith("audio/")) return "audio";
  if (mimeType.startsWith("video/")) return "video";
  return "file";
}

function newId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `attachment-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function readFileData(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(reader.error ?? new Error("attachment read failed"));
    reader.readAsDataURL(file);
  });
}

function fileToStage(file: File, origin: StagedAttachment["origin"]): Promise<StagedAttachment | null> {
  if (!accepts(file.type)) return Promise.resolve(null);
  if (file.size > MAX_STAGED_ATTACHMENT_BYTES) return Promise.resolve(null);
  return readFileData(file).then((dataUrl) => {
    const comma = dataUrl.indexOf(",");
    const data = comma >= 0 ? dataUrl.slice(comma + 1) : "";
    const id = newId();
    return {
      id,
      name: file.name || "attachment",
      size: file.size,
      mimeType: file.type || "application/octet-stream",
      kind: kindFor(file.type),
      state: "ready",
      data,
      origin,
      previewUrl: file.type.startsWith("image/") ? dataUrl : undefined,
    } satisfies StagedAttachment;
  });
}

function positiveInt(value: unknown): number | undefined {
  return typeof value === "number" && value > 0 ? Math.floor(value) : undefined;
}

/** Extract custom in-app paths and native files synchronously, like Hermes. */
export function extractDroppedFiles(transfer: DataTransfer): DroppedFile[] {
  const result: DroppedFile[] = [];
  const seenPaths = new Set<string>();
  const seenFiles = new Set<File>();
  let internal: unknown;
  try {
    internal = JSON.parse(transfer.getData(HERMES_PATHS_MIME) || "null");
  } catch {
    internal = null;
  }
  if (Array.isArray(internal)) {
    for (const entry of internal) {
      if (!entry || typeof entry.path !== "string" || !entry.path) continue;
      const line = positiveInt(entry.line);
      const rawEnd = positiveInt(entry.lineEnd);
      const lineEnd = line && rawEnd && rawEnd > line ? rawEnd : undefined;
      const key = line ? `${entry.path}:${line}-${lineEnd ?? line}` : entry.path;
      if (seenPaths.has(key)) continue;
      seenPaths.add(key);
      result.push({
        path: entry.path,
        isDirectory: entry.isDirectory === true,
        line,
        lineEnd,
      });
    }
  }

  const pushNative = (file: File, isDirectory: boolean) => {
    if (seenFiles.has(file)) return;
    seenFiles.add(file);
    const path = (file as File & { path?: string; webkitRelativePath?: string }).path ||
      file.webkitRelativePath || "";
    if (path && seenPaths.has(path)) return;
    if (path) seenPaths.add(path);
    if (isDirectory) {
      if (path) result.push({ path, isDirectory: true });
    } else {
      result.push({ file, path });
    }
  };

  const items = transfer.items;
  if (items) {
    for (let index = 0; index < items.length; index += 1) {
      const item = items[index];
      if (!item || item.kind !== "file") continue;
      let directory = false;
      try {
        const entry = (item as DataTransferItem & { webkitGetAsEntry?: () => { isDirectory?: boolean } | null }).webkitGetAsEntry?.();
        directory = entry?.isDirectory === true;
      } catch {
        directory = false;
      }
      const file = item.getAsFile();
      if (file) pushNative(file, directory);
    }
  }
  for (let index = 0; index < (transfer.files?.length ?? 0); index += 1) {
    const file = transfer.files.item(index);
    if (file) pushNative(file, false);
  }
  return result;
}

/** File-bearing entries are OS/Finder drops; path-only entries are in-app refs. */
export function partitionDroppedFiles(candidates: DroppedFile[]): {
  osDrops: DroppedFile[];
  inAppRefs: DroppedFile[];
} {
  return candidates.reduce(
    (groups, candidate) => {
      (candidate.file ? groups.osDrops : groups.inAppRefs).push(candidate);
      return groups;
    },
    { osDrops: [], inAppRefs: [] } as { osDrops: DroppedFile[]; inAppRefs: DroppedFile[] },
  );
}

function quoteRef(value: string): string {
  if (!/[\s()[\]{}<>"'`]/.test(value)) return value;
  if (!value.includes("`")) return `\`${value}\``;
  if (!value.includes('"')) return `"${value}"`;
  if (!value.includes("'")) return `'${value}'`;
  return value;
}

export function droppedFileInlineRef(candidate: DroppedFile, workspaceRoot = ""): string | null {
  if (!candidate.path) return null;
  const root = workspaceRoot.replace(/\\/g, "/").replace(/\/+$/, "");
  const path = candidate.path.replace(/\\/g, "/");
  const relative = root && (path === root || path.startsWith(`${root}/`)) ? path.slice(root.length + 1) : path;
  if (candidate.line) {
    const range = candidate.lineEnd && candidate.lineEnd > candidate.line
      ? `${candidate.line}-${candidate.lineEnd}`
      : `${candidate.line}`;
    return `@line:${quoteRef(`${relative}:${range}`)}`;
  }
  return `@${candidate.isDirectory ? "folder" : "file"}:${quoteRef(relative)}`;
}

export function droppedFileInlineRefs(candidates: DroppedFile[], workspaceRoot = ""): string[] {
  return candidates
    .map((candidate) => droppedFileInlineRef(candidate, workspaceRoot))
    .filter((ref): ref is string => Boolean(ref));
}

export function toDaemonAttachments(attachments: readonly StagedAttachment[]): Attachment[] {
  return attachments
    .filter((item) => item.kind === "image" && item.state === "ready" && item.data)
    .map((item) => ({
      type: "image" as const,
      data: item.data!,
      mimeType: item.mimeType,
      id: item.id,
      filename: item.name,
      sizeBytes: item.size,
    }));
}

export function useAttachmentStaging({
  workspaceRoot = "",
  maxAttachments = MAX_STAGED_ATTACHMENTS,
  onInsertInlineRefs,
  onAttachDroppedItems,
  onStatus,
  requestMainFocus,
}: UseAttachmentStagingOptions = {}): AttachmentStagingProjection {
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  const [dragActive, setDragActive] = useState(false);
  const dragDepthRef = useRef(0);

  const addFiles = useCallback(async (files: File[]) => {
    const available = Math.max(0, maxAttachments - attachments.length);
    const staged = (await Promise.all(files.slice(0, available).map((file) => fileToStage(file, "local"))))
      .filter((item): item is StagedAttachment => item !== null);
    if (staged.length) {
      setAttachments((current) => [...current, ...staged].slice(0, maxAttachments));
      onStatus?.(`${staged.length} attachment${staged.length === 1 ? "" : "s"} ready`);
    } else if (files.length) {
      onStatus?.("No supported attachments found");
    }
    return staged;
  }, [attachments.length, maxAttachments, onStatus]);

  const attachDroppedItems = useCallback(async (items: DroppedFile[]) => {
    const { inAppRefs, osDrops } = partitionDroppedFiles(items);
    const refs = droppedFileInlineRefs(inAppRefs, workspaceRoot);
    if (refs.length) onInsertInlineRefs?.(refs);
    if (!osDrops.length) return refs.length > 0;
    if (onAttachDroppedItems) {
      const accepted = await onAttachDroppedItems(osDrops);
      if (accepted) requestMainFocus?.();
      return accepted;
    }
    const files = osDrops.map((item) => item.file).filter((file): file is File => Boolean(file));
    const staged = await addFiles(files);
    if (staged.length) requestMainFocus?.();
    return staged.length > 0;
  }, [addFiles, onAttachDroppedItems, onInsertInlineRefs, requestMainFocus, workspaceRoot]);

  const resetDragState = useCallback(() => {
    dragDepthRef.current = 0;
    setDragActive(false);
  }, []);

  const handleDragEnter = useCallback((event: React.DragEvent<HTMLElement>) => {
    if (!event.dataTransfer.types.includes(HERMES_PATHS_MIME) && !event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    dragDepthRef.current += 1;
    setDragActive(true);
  }, []);
  const handleDragLeave = useCallback((event: React.DragEvent<HTMLElement>) => {
    event.preventDefault();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (!dragDepthRef.current) setDragActive(false);
  }, []);
  const handleDragOver = useCallback((event: React.DragEvent<HTMLElement>) => {
    if (!event.dataTransfer.types.includes(HERMES_PATHS_MIME) && !event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "copy";
  }, []);
  const handleDrop = useCallback((event: React.DragEvent<HTMLElement>) => {
    event.preventDefault();
    resetDragState();
    const candidates = extractDroppedFiles(event.dataTransfer);
    if (candidates.length) void attachDroppedItems(candidates);
  }, [attachDroppedItems, resetDragState]);
  const handleInputDragOver = useCallback((event: React.DragEvent<HTMLElement>) => {
    if (!event.dataTransfer.types.includes(HERMES_PATHS_MIME) && !event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
  }, []);
  const handleInputDrop = useCallback((event: React.DragEvent<HTMLElement>) => {
    if (!event.dataTransfer.types.includes(HERMES_PATHS_MIME) && !event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    event.stopPropagation();
    resetDragState();
    const candidates = extractDroppedFiles(event.dataTransfer);
    if (candidates.length) void attachDroppedItems(candidates);
  }, [attachDroppedItems, resetDragState]);
  const handlePaste = useCallback((event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData.items)
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => Boolean(file));
    if (!files.length) return;
    event.preventDefault();
    void addFiles(files);
  }, [addFiles]);

  return {
    attachments,
    stagedAttachments: attachments,
    dragActive,
    addFiles,
    attachDroppedItems,
    removeAttachment: (id) => setAttachments((current) => current.filter((item) => item.id !== id)),
    clearAttachments: () => setAttachments([]),
    restoreAttachments: (items) => setAttachments(items.map((item) => ({
      id: item.id,
      name: item.name,
      size: item.size ?? 0,
      mimeType: item.mimeType,
      kind: item.kind ?? kindFor(item.mimeType),
      state: item.state ?? "lost",
      path: item.path,
      error: item.error ?? "attachment bytes unavailable — re-attach to resend",
      origin: item.origin ?? "remote",
    }))),
    handlePaste,
    handleDragEnter,
    handleDragLeave,
    handleDragOver,
    handleDrop,
    handleInputDragOver,
    handleInputDrop,
  };
}
