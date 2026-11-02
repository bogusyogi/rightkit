import type { LoadedDocument } from "../documents/types.js";

// IPC-boundary DOCX disk, confined to disposable QA profile. Readback exposes only saved bytes.
const PREFIX = "viewright.qa.docx-disk:";
type SavedDocx = {
  contentBase64: string;
  sourceSha256: string;
  byteLength: number;
  mtimeMs: number;
};
function read(path: string): SavedDocx | null {
  const raw = sessionStorage.getItem(PREFIX + path);
  return raw ? (JSON.parse(raw) as SavedDocx) : null;
}
function publishReadback() {
  (window as unknown as { __viewrightQaDocx: { read: typeof read } }).__viewrightQaDocx =
    Object.freeze({ read });
}
export function readQaDocxDocument(doc: LoadedDocument): LoadedDocument | null {
  if (doc.format !== "docx") return null;
  const entry = read(doc.path);
  if (!entry) return null;
  publishReadback();
  return {
    ...doc,
    content: entry.contentBase64,
    assetUrl: undefined,
    fingerprint: { mtimeMs: entry.mtimeMs, size: entry.byteLength },
  };
}
export async function writeQaDocxDocument(
  path: string,
  bytes: ArrayBuffer,
  contentBase64: string,
): Promise<void> {
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  const sourceSha256 = Array.from(new Uint8Array(hash), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
  sessionStorage.setItem(
    PREFIX + path,
    JSON.stringify({
      contentBase64,
      sourceSha256,
      byteLength: bytes.byteLength,
      mtimeMs: Date.now(),
    } satisfies SavedDocx),
  );
  publishReadback();
}
