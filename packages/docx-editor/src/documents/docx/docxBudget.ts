import { hostAppName } from "../../appName.js";
/**
 * Pre-expansion budget for DOCX (ZIP) input. The container is already capped at 150 MB compressed
 * (src-tauri documents.rs MAX_BINARY_BYTES), but the vendored JSZip inflates with no aggregate or
 * ratio limit, so a small "zip bomb" DOCX could exhaust memory. This reads only the ZIP central
 * directory (a few KB, no inflation) and refuses a document whose declared sizes exceed the budget.
 * JSZip itself rejects entries whose real inflated size differs from the declared one, so the
 * declared sizes are what bound expansion.
 */

/** Total declared uncompressed bytes across all entries. Real documents are dominated by
 *  already-compressed media (ratio ~1) under the 150 MB container cap; 512 MiB leaves >3x headroom
 *  for text-heavy files while keeping worst-case webview memory bounded. */
export const DOCX_MAX_UNCOMPRESSED_BYTES = 512 * 1024 * 1024;
/** Any single entry (e.g. one giant document.xml or embedded bitmap). */
export const DOCX_MAX_ENTRY_BYTES = 256 * 1024 * 1024;
/** Overall expansion ratio, only enforced once expansion exceeds the floor below so tiny
 *  highly-repetitive files (blank templates compress >100:1) are never refused. Typical XML is
 *  ~10:1 and extreme legitimate repetitive documents stay under ~100:1. */
export const DOCX_MAX_RATIO = 200;
export const DOCX_RATIO_FLOOR_BYTES = 64 * 1024 * 1024;
/** Entry-count ceiling (real documents have tens to low thousands of parts). */
export const DOCX_MAX_ENTRIES = 20000;

export class DocxBudgetError extends Error {
  readonly code = "docx-over-budget";
  constructor(message: string) {
    super(message);
    this.name = "DocxBudgetError";
  }
}

const mib = (n: number) => `${Math.round(n / (1024 * 1024))} MB`;

/** Throws DocxBudgetError (with a user-readable message) when the ZIP exceeds the budget.
 *  Malformed or non-ZIP input is left for the editor to report. */
export function assertDocxWithinBudget(buffer: ArrayBuffer): void {
  const bytes = new Uint8Array(buffer);
  const view = new DataView(buffer);
  const len = bytes.length;
  if (len < 22) return;
  // End-of-central-directory record: scan back through the max 64 KiB comment.
  let eocd = -1;
  for (let i = len - 22; i >= Math.max(0, len - 22 - 0xffff); i -= 1) {
    if (view.getUint32(i, true) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) return;
  const total = view.getUint16(eocd + 10, true);
  const cdSize = view.getUint32(eocd + 12, true);
  const cdOffset = view.getUint32(eocd + 16, true);
  if (total === 0xffff || cdSize === 0xffffffff || cdOffset === 0xffffffff) {
    throw new DocxBudgetError(
      `this Word document uses ZIP64 packaging, which is larger than ${hostAppName()} will expand safely`,
    );
  }
  if (total > DOCX_MAX_ENTRIES) {
    throw new DocxBudgetError(
      `this Word document contains ${total} parts (limit ${DOCX_MAX_ENTRIES}); refusing to expand it`,
    );
  }
  let pos = cdOffset;
  let sumCompressed = 0;
  let sumUncompressed = 0;
  for (let n = 0; n < total; n += 1) {
    if (pos + 46 > len || view.getUint32(pos, true) !== 0x02014b50) return; // malformed: editor reports
    const compressed = view.getUint32(pos + 20, true);
    const uncompressed = view.getUint32(pos + 24, true);
    const nameLen = view.getUint16(pos + 28, true);
    const extraLen = view.getUint16(pos + 30, true);
    const commentLen = view.getUint16(pos + 32, true);
    if (compressed === 0xffffffff || uncompressed === 0xffffffff) {
      throw new DocxBudgetError(
        `this Word document contains a part larger than ${hostAppName()} will expand safely`,
      );
    }
    if (uncompressed > DOCX_MAX_ENTRY_BYTES) {
      throw new DocxBudgetError(
        `this Word document contains a ${mib(uncompressed)} part (limit ${mib(DOCX_MAX_ENTRY_BYTES)}); refusing to expand it`,
      );
    }
    sumCompressed += compressed;
    sumUncompressed += uncompressed;
    if (sumUncompressed > DOCX_MAX_UNCOMPRESSED_BYTES) {
      throw new DocxBudgetError(
        `this Word document expands to more than ${mib(DOCX_MAX_UNCOMPRESSED_BYTES)}; refusing to expand it`,
      );
    }
    pos += 46 + nameLen + extraLen + commentLen;
  }
  if (
    sumUncompressed > DOCX_RATIO_FLOOR_BYTES &&
    sumUncompressed > Math.max(sumCompressed, 1) * DOCX_MAX_RATIO
  ) {
    throw new DocxBudgetError(
      `this Word document expands more than ${DOCX_MAX_RATIO}:1 (${mib(sumUncompressed)} from ${mib(sumCompressed)}); refusing to expand it`,
    );
  }
}
