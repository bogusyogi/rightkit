import type { DriveFile } from "@rightkit/mail-ui/lib/ipc";

/** File-type to recognizable glyph plus Google's semantic color, or null for themed. */
export function fileGlyph(f: DriveFile): { icon: string; color: string | null } {
  if (f.is_folder) return { icon: "folder", color: null };
  const m = f.mime_type;
  if (m.includes("application/vnd.google-apps.document")) return { icon: "doc", color: "#4285F4" };
  if (m.includes("spreadsheet")) return { icon: "doc", color: "#0F9D58" };
  if (m.includes("presentation")) return { icon: "doc", color: "#F4B400" };
  if (m === "application/pdf") return { icon: "doc", color: "#DB4437" };
  if (m.startsWith("image/")) return { icon: "image", color: null };
  if (m.startsWith("video/")) return { icon: "video", color: null };
  return { icon: "draft", color: null };
}
