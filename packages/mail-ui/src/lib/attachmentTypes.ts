const WEAK_MIME_TYPES = new Set([
  "",
  "application/octet-stream",
  "binary/octet-stream",
  "application/unknown",
]);

const EXTENSION_MIME: Record<string, string> = {
  bmp: "image/bmp",
  csv: "text/csv",
  gif: "image/gif",
  jpeg: "image/jpeg",
  jpg: "image/jpeg",
  json: "application/json",
  log: "text/plain",
  md: "text/markdown",
  ods: "application/vnd.oasis.opendocument.spreadsheet",
  pdf: "application/pdf",
  png: "image/png",
  svg: "image/svg+xml",
  text: "text/plain",
  tif: "image/tiff",
  tiff: "image/tiff",
  ts: "text/plain",
  tsv: "text/tab-separated-values",
  txt: "text/plain",
  webp: "image/webp",
  xls: "application/vnd.ms-excel",
  xlsb: "application/vnd.ms-excel.sheet.binary.macroenabled.12",
  xlsm: "application/vnd.ms-excel.sheet.macroenabled.12",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  xml: "application/xml",
};

export type AttachmentTone =
  | "archive"
  | "doc"
  | "generic"
  | "image"
  | "pdf"
  | "sheet"
  | "slides"
  | "text";

export interface AttachmentTypeInfo {
  ext: string;
  label: string;
  mime: string;
  iconName: "doc" | "image";
  tone: AttachmentTone;
}

export function normalizedMime(mimeType: string | null | undefined): string {
  return (mimeType ?? "").split(";")[0].trim().toLowerCase();
}

export function extensionOf(filename: string): string {
  const clean = filename.split(/[?#]/)[0] ?? filename;
  const dot = clean.lastIndexOf(".");
  return dot >= 0 ? clean.slice(dot + 1).toLowerCase() : "";
}

function mimeFromBytes(bytes?: Uint8Array): string | null {
  if (!bytes || bytes.length < 4) return null;
  if (
    bytes.length >= 8 &&
    bytes[0] === 0x89 &&
    bytes[1] === 0x50 &&
    bytes[2] === 0x4e &&
    bytes[3] === 0x47 &&
    bytes[4] === 0x0d &&
    bytes[5] === 0x0a &&
    bytes[6] === 0x1a &&
    bytes[7] === 0x0a
  ) {
    return "image/png";
  }
  if (bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    bytes.length >= 6 &&
    bytes[0] === 0x47 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x38 &&
    (bytes[4] === 0x37 || bytes[4] === 0x39) &&
    bytes[5] === 0x61
  ) {
    return "image/gif";
  }
  if (
    bytes.length >= 12 &&
    bytes[0] === 0x52 &&
    bytes[1] === 0x49 &&
    bytes[2] === 0x46 &&
    bytes[3] === 0x46 &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp";
  }
  if (bytes[0] === 0x42 && bytes[1] === 0x4d) return "image/bmp";
  if (
    bytes[0] === 0x25 &&
    bytes[1] === 0x50 &&
    bytes[2] === 0x44 &&
    bytes[3] === 0x46
  ) {
    return "application/pdf";
  }
  return null;
}

function mimeFromExtension(filename: string): string | null {
  return EXTENSION_MIME[extensionOf(filename)] ?? null;
}

export function inferAttachmentMime(
  filename: string,
  mimeType: string | null | undefined,
  bytes?: Uint8Array,
): string {
  const mime = normalizedMime(mimeType);
  if (!WEAK_MIME_TYPES.has(mime)) return mime;
  return (
    mimeFromBytes(bytes) ??
    mimeFromExtension(filename) ??
    mime ??
    "application/octet-stream"
  );
}

export function isPdfAttachment(
  filename: string,
  mimeType: string | null | undefined,
  bytes?: Uint8Array,
): boolean {
  return (
    inferAttachmentMime(filename, mimeType, bytes) === "application/pdf" ||
    extensionOf(filename) === "pdf"
  );
}

export function isSpreadsheetAttachment(
  filename: string,
  mimeType: string | null | undefined,
): boolean {
  const mime = inferAttachmentMime(filename, mimeType);
  const ext = extensionOf(filename);
  return (
    mime.includes("spreadsheet") ||
    mime === "application/vnd.ms-excel" ||
    mime === "text/csv" ||
    ["csv", "tsv", "xls", "xlsx", "xlsm", "xlsb", "ods"].includes(ext)
  );
}

/**
 * True when a spreadsheet attachment can be inlined in the preview modal
 * without parsing untrusted bytes through a vulnerable library. Only the
 * plain-text tabular formats (CSV, TSV) qualify — the binary spreadsheet
 * formats (.xlsx/.xls/.xlsm/.xlsb/.ods) are kept download-only while the
 * xlsx 0.18.5 advisories remain, because parsing untrusted Gmail attachment
 * bytes with that library is the audit-flagged attack surface.
 */
export function isInlinePreviewableSpreadsheet(
  filename: string,
  mimeType: string | null | undefined,
): boolean {
  const mime = inferAttachmentMime(filename, mimeType);
  const ext = extensionOf(filename);
  return (
    mime === "text/csv" ||
    mime === "text/tab-separated-values" ||
    ext === "csv" ||
    ext === "tsv"
  );
}

export function isTextLikeAttachment(
  filename: string,
  mimeType: string | null | undefined,
): boolean {
  const mime = inferAttachmentMime(filename, mimeType);
  const ext = extensionOf(filename);
  return (
    mime.startsWith("text/") ||
    mime.includes("json") ||
    mime.includes("xml") ||
    ["txt", "log", "json", "xml", "md", "ics", "vcf"].includes(ext)
  );
}

export function isRenderableImageAttachment(
  filename: string,
  mimeType: string | null | undefined,
  bytes?: Uint8Array,
): boolean {
  return [
    "image/bmp",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
  ].includes(inferAttachmentMime(filename, mimeType, bytes));
}

export function attachmentTypeInfo(
  filename: string,
  mimeType: string | null | undefined = "",
  bytes?: Uint8Array,
): AttachmentTypeInfo {
  const ext = extensionOf(filename);
  const mime = inferAttachmentMime(filename, mimeType, bytes);
  const rawLabel = ext || mime.split("/").pop() || "file";
  const label =
    rawLabel === "jpeg" ? "JPG" : rawLabel === "spreadsheetml.sheet" ? "XLS" : rawLabel;
  let tone: AttachmentTone = "generic";
  let iconName: "doc" | "image" = "doc";

  if (isRenderableImageAttachment(filename, mime, bytes)) {
    tone = "image";
    iconName = "image";
  } else if (isPdfAttachment(filename, mime, bytes)) {
    tone = "pdf";
  } else if (isSpreadsheetAttachment(filename, mime)) {
    tone = "sheet";
  } else if (mime.includes("presentation") || ["ppt", "pptx"].includes(ext)) {
    tone = "slides";
  } else if (mime.includes("word") || ["doc", "docx"].includes(ext)) {
    tone = "doc";
  } else if (isTextLikeAttachment(filename, mime)) {
    tone = "text";
  } else if (["7z", "gz", "rar", "tar", "zip"].includes(ext)) {
    tone = "archive";
  }

  return {
    ext,
    label: label.slice(0, 4).toUpperCase(),
    mime,
    iconName,
    tone,
  };
}
