import type { LoadedDocument } from "../documents/types.js";

// IPC-boundary disk for browser journeys. Session storage belongs to disposable QA
// browser profile; survives reload, never reads/writes native app data or user files.
const PREFIX = "viewright.qa.text-disk:";
type DiskEntry = { content: string; encoding: string; sourceSha256: string };

function diskKey(path: string): string {
  return (
    PREFIX +
    (new URLSearchParams(location.search).get("qaQuality") === "1" ? "quality:" : "default:") +
    path
  );
}
function read(path: string): DiskEntry | null {
  const raw = sessionStorage.getItem(diskKey(path));
  return raw ? (JSON.parse(raw) as DiskEntry) : null;
}
async function digest(content: string): Promise<string> {
  const bytes = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(content));
  return Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
function publishReadback() {
  (window as unknown as { __viewrightQaText: { read: typeof read } }).__viewrightQaText = { read };
}
export async function readQaTextDocument(doc: LoadedDocument): Promise<LoadedDocument> {
  if (!["txt", "md", "mdx", "html"].includes(doc.format)) return doc;
  const content =
    doc.format === "md" && new URLSearchParams(location.search).get("qaQuality") === "1"
      ? doc.content + "\n\n    protected code\n      second line\n"
      : doc.content;
  const entry = read(doc.path) ?? {
    content,
    encoding: doc.encoding ?? "UTF-8",
    sourceSha256: await digest(content),
  };
  sessionStorage.setItem(diskKey(doc.path), JSON.stringify(entry));
  publishReadback();
  return {
    ...doc,
    ...entry,
    fingerprint: { mtimeMs: 1, size: new TextEncoder().encode(entry.content).length },
  };
}
export async function writeQaTextDocument(
  path: string,
  content: string,
  encoding = "UTF-8",
  expected?: string,
) {
  const previous = read(path);
  if (expected && previous && expected !== previous.sourceSha256) {
    throw new Error("QA disk changed before save");
  }
  const entry = { content, encoding, sourceSha256: await digest(content) };
  sessionStorage.setItem(diskKey(path), JSON.stringify(entry));
  publishReadback();
  return { encoding, sourceSha256: entry.sourceSha256 };
}
