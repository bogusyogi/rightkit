import type { MutableRefObject } from "react";
import type { HtmlPageBuffer, HtmlSessionPages } from "@rightkit/html-editor/documents/html/sessionTypes";
import { editableImageOutputForPath, type ImageSaveFunction } from "@rightkit/media-ui";
import { bufferToBase64 } from "@rightkit/docx-editor/documents/docx/blankDocx";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import {
  clearSetupRecovery as clearNativeSetupRecovery,
  loadSetupRecovery as loadNativeSetupRecovery,
  saveSetupRecovery as saveNativeSetupRecovery,
} from "@rightkit/document-core/state/tauriBridge";

export interface SetupRecoveryEntry {
  document: Omit<LoadedDocument, "assetUrl" | "tabKey">;
  view?: ViewMode;
  editedText?: string;
  htmlPages?: Record<string, HtmlPageBuffer>;
  binary?: { base64: string; mime: string };
}

export interface SetupRecoverySnapshot {
  version: 1;
  activePath: string | null;
  documents: SetupRecoveryEntry[];
}

interface CaptureSetupRecoveryDeps {
  openDocs: LoadedDocument[];
  activePath: string | null;
  views: Record<string, ViewMode>;
  isDirty: (doc: LoadedDocument) => boolean;
  editsRef: MutableRefObject<Record<string, string>>;
  htmlPagesRef: MutableRefObject<HtmlSessionPages>;
  docxSaversRef: MutableRefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  imageSaversRef: MutableRefObject<Map<string, ImageSaveFunction>>;
}

function storableDocument(doc: LoadedDocument): SetupRecoveryEntry["document"] {
  const { assetUrl: _assetUrl, tabKey: _tabKey, ...stored } = doc;
  return stored;
}

export async function captureSetupRecovery({
  openDocs,
  activePath,
  views,
  isDirty,
  editsRef,
  htmlPagesRef,
  docxSaversRef,
  imageSaversRef,
}: CaptureSetupRecoveryDeps): Promise<SetupRecoverySnapshot> {
  const dirtyDocs = openDocs.filter(isDirty);
  const dirtyPdf = dirtyDocs.find((doc) => doc.format === "pdf");
  if (dirtyPdf) {
    // PDF source commands & annotations are not represented by the text snapshot channel.
    // Keep the running window and edits intact instead of replacing it with incomplete recovery.
    throw new Error(`Save or close ${dirtyPdf.displayName} before installing the update.`);
  }
  const documents = await Promise.all(
    dirtyDocs.map(async (doc): Promise<SetupRecoveryEntry> => {
      const entry: SetupRecoveryEntry = {
        document: storableDocument(doc),
        view: views[doc.path],
      };
      if (doc.format === "docx") {
        const buffer = await docxSaversRef.current.get(doc.path)?.();
        if (!buffer) throw new Error(`Could not preserve ${doc.displayName}.`);
        entry.binary = {
          base64: bufferToBase64(buffer),
          mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        };
      } else if (doc.format === "image") {
        const saver = imageSaversRef.current.get(doc.path);
        const payload = await saver?.(editableImageOutputForPath(doc.path) ?? "png");
        if (!payload) throw new Error(`Could not preserve ${doc.displayName}.`);
        entry.binary = { base64: bufferToBase64(payload.buffer), mime: payload.mime };
      } else {
        entry.editedText = editsRef.current[doc.path] ?? doc.content;
        if (doc.format === "html" && htmlPagesRef.current[doc.path]) {
          entry.htmlPages = htmlPagesRef.current[doc.path];
        }
      }
      return entry;
    }),
  );
  return { version: 1, activePath, documents };
}

function isSetupRecoverySnapshot(value: unknown): value is SetupRecoverySnapshot {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<SetupRecoverySnapshot>;
  return candidate.version === 1 && Array.isArray(candidate.documents);
}

export async function saveSetupRecoverySnapshot(snapshot: SetupRecoverySnapshot): Promise<void> {
  await saveNativeSetupRecovery(JSON.stringify(snapshot));
}

export async function loadSetupRecoverySnapshot(): Promise<SetupRecoverySnapshot | null> {
  const raw = await loadNativeSetupRecovery();
  if (!raw) return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    return isSetupRecoverySnapshot(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export async function clearSetupRecoverySnapshot(): Promise<void> {
  await clearNativeSetupRecovery();
}
