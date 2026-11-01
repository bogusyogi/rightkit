import { useEffect, useMemo, useState } from "react";
import { Button, IconButton, ModalShell, Table, TableBody, TableCell, TableRow } from "@rightkit/react-shell/react";
import AttachmentTypeIcon from "./AttachmentTypeIcon";
import Icon from "./Icons";
import { AttachmentPdfPreview } from "./AttachmentPdfPreview";
import {
  inferAttachmentMime,
  isInlinePreviewableSpreadsheet,
  isPdfAttachment,
  isRenderableImageAttachment,
  isSpreadsheetAttachment,
  isTextLikeAttachment,
} from "../lib/attachmentTypes";

export interface AttachmentPreviewPayload {
  filename: string;
  mimeType: string;
  dataB64: string;
}

interface Props {
  preview: AttachmentPreviewPayload;
  onClose: () => void;
  onDownload: () => void;
}

type SheetPreview = {
  sheetName: string;
  rows: string[][];
  totalRows: number;
  totalCols: number;
};

const MAX_SHEET_ROWS = 120;
const MAX_SHEET_COLS = 32;
const MAX_TEXT_CHARS = 180_000;
/** xlsx has unpatched HIGH CVEs — skip parse for files over this threshold. */
const MAX_SPREADSHEET_BYTES = 5 * 1024 * 1024; // 5 MB

function bytesFromBase64(dataB64: string): Uint8Array {
  const binary = atob(dataB64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function decodeText(bytes: Uint8Array): string {
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

async function buildSheetPreview(
  bytes: Uint8Array,
  filename: string,
  mimeType: string,
): Promise<SheetPreview> {
  // Security gate: xlsx 0.18.5 carries two unpatched HIGH-severity advisories
  // and pnpm audit --prod flags it. Until we swap parsers or isolate parsing
  // in a sandbox, the inline preview is restricted to plain-text tabular
  // formats (CSV/TSV) that we can parse with the Web Platform's TextDecoder
  // + a tiny split — never touching the vulnerable library. Binary
  // spreadsheet formats (.xlsx/.xls/.xlsm/.xlsb/.ods) are download-only;
  // `isInlinePreviewableSpreadsheet` upstream keeps them out of this code
  // path entirely, so the dynamic `await import("xlsx")` below is dead code
  // until the library is replaced.
  if (bytes.length > MAX_SPREADSHEET_BYTES) {
    throw new Error(
      `Preview unavailable for files over ${MAX_SPREADSHEET_BYTES / (1024 * 1024)} MB.`,
    );
  }
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  const isTsv =
    ext === "tsv" || mimeType === "text/tab-separated-values";
  // Bare-bones CSV/TSV parser: split on the row separator, then on the
  // delimiter. Quote handling is intentionally minimal — inline preview
  // is best-effort, and the user can download the file for full fidelity.
  const text = decodeText(bytes);
  const delimiter = isTsv ? "\t" : ",";
  const lines = text.split(/\r\n|\n|\r/);
  const rows: string[][] = [];
  let maxCols = 0;
  for (let i = 0; i < lines.length && i < MAX_SHEET_ROWS; i += 1) {
    const line = lines[i];
    if (!line) {
      rows.push([]);
      continue;
    }
    const cells = line.split(delimiter).slice(0, MAX_SHEET_COLS);
    maxCols = Math.max(maxCols, cells.length);
    rows.push(cells);
  }
  const totalRows = lines.filter((l) => l.length > 0).length;
  return {
    sheetName: ext === "tsv" ? "Sheet 1 (TSV)" : "Sheet 1 (CSV)",
    rows,
    totalRows,
    totalCols: maxCols,
  };
}

function SpreadsheetPreview({ sheet }: { sheet: SheetPreview }) {
  if (sheet.rows.length === 0) {
    return (
      <div className="attachment-preview-fallback">
        <AttachmentTypeIcon
          filename={`${sheet.sheetName}.xlsx`}
          mimeType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
          size="lg"
        />
        <span>This spreadsheet is empty.</span>
      </div>
    );
  }
  return (
    <div className="attachment-sheet-preview">
      <div className="attachment-sheet-head">
        <span>{sheet.sheetName}</span>
        {(sheet.totalRows > MAX_SHEET_ROWS || sheet.totalCols > MAX_SHEET_COLS) && (
          <span>
            Showing {Math.min(sheet.totalRows, MAX_SHEET_ROWS)} of {sheet.totalRows} rows
            {sheet.totalCols > MAX_SHEET_COLS ? `, first ${MAX_SHEET_COLS} columns` : ""}
          </span>
        )}
      </div>
      <div className="attachment-sheet-scroll">
        <Table label={sheet.sheetName}>
          <TableBody>
            {sheet.rows.map((row, rowIndex) => (
              <TableRow key={rowIndex}>
                {row.map((cell, colIndex) => (
                  <TableCell key={colIndex}>{cell}</TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  );
}

export function AttachmentPreviewModal({ preview, onClose, onDownload }: Props) {
  const bytes = useMemo(() => bytesFromBase64(preview.dataB64), [preview.dataB64]);
  const effectiveMime = useMemo(
    () => inferAttachmentMime(preview.filename, preview.mimeType, bytes),
    [bytes, preview.filename, preview.mimeType],
  );
  const renderableImage = useMemo(
    () => isRenderableImageAttachment(preview.filename, effectiveMime, bytes),
    [bytes, effectiveMime, preview.filename],
  );
  const pdfPreview = useMemo(
    () => isPdfAttachment(preview.filename, effectiveMime, bytes),
    [bytes, effectiveMime, preview.filename],
  );
  // Inline spreadsheet preview is intentionally restricted to plain-text
  // tabular formats (CSV/TSV). Binary spreadsheet formats (.xlsx/.xls/.ods/…)
  // are still recognised as spreadsheet attachments for the icon/chip, but
  // they must be downloaded to open — parsing them through xlsx 0.18.5 hits
  // the audit-flagged CVE surface against untrusted Gmail attachment bytes.
  const spreadsheetPreview = useMemo(
    () => isInlinePreviewableSpreadsheet(preview.filename, effectiveMime),
    [effectiveMime, preview.filename],
  );
  const isBinarySpreadsheet = useMemo(
    () =>
      isSpreadsheetAttachment(preview.filename, effectiveMime) &&
      !isInlinePreviewableSpreadsheet(preview.filename, effectiveMime),
    [effectiveMime, preview.filename],
  );
  const sheetKey = `${preview.filename}:${preview.dataB64.length}`;
  const [sheetResult, setSheetResult] = useState<{
    key: string;
    sheet: SheetPreview | null;
    error: string | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (!spreadsheetPreview) return;
    buildSheetPreview(bytes, preview.filename, effectiveMime)
      .then((next) => {
        if (cancelled) return;
        setSheetResult({ key: sheetKey, sheet: next, error: null });
      })
      .catch((e) => {
        if (cancelled) return;
        setSheetResult({
          key: sheetKey,
          sheet: null,
          error: e instanceof Error ? e.message : String(e),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [bytes, effectiveMime, sheetKey, spreadsheetPreview, preview.filename]);

  const currentSheetResult = sheetResult?.key === sheetKey ? sheetResult : null;
  const sheet = currentSheetResult?.sheet ?? null;
  const sheetError = currentSheetResult?.error ?? null;
  const sheetStatus = spreadsheetPreview && !currentSheetResult ? "Loading spreadsheet..." : "";

  const textPreview = useMemo(() => {
    if (!isTextLikeAttachment(preview.filename, effectiveMime)) return null;
    const text = decodeText(bytes);
    return text.length > MAX_TEXT_CHARS
      ? `${text.slice(0, MAX_TEXT_CHARS)}\n\n[Preview truncated]`
      : text;
  }, [bytes, effectiveMime, preview.filename]);

  const imageSrc = useMemo(
    () => `data:${effectiveMime};base64,${preview.dataB64}`,
    [effectiveMime, preview.dataB64],
  );

  return (
    <ModalShell
      label={preview.filename}
      onClose={onClose}
      width={1040}
      dismissOn="click"
      className="attachment-preview-modal"
      overlayClassName="attachment-preview-overlay"
    >
      <div className="attachment-preview-head">
        <AttachmentTypeIcon
          filename={preview.filename}
          mimeType={effectiveMime}
          bytes={bytes}
          size="lg"
        />
        <span className="attachment-preview-title">{preview.filename}</span>
        <IconButton label={`Download ${preview.filename}`}
          className="attachment-preview-action"
          title={`Download ${preview.filename}`}
          aria-label={`Download ${preview.filename}`}
          onClick={onDownload}
        >
          <Icon name="download" size={17} />
        </IconButton>
        <IconButton label="Close preview"
          className="attachment-preview-close"
          title="Close preview"
          aria-label="Close preview"
          onClick={onClose}
        >
          x
        </IconButton>
      </div>
      <div className="attachment-preview-body">
        {renderableImage ? (
          <img
            className="attachment-preview-img"
            src={imageSrc}
            alt={preview.filename}
          />
        ) : pdfPreview ? (
          <AttachmentPdfPreview
            key={`${preview.filename}:${preview.dataB64.length}`}
            bytes={bytes}
            filename={preview.filename}
          />
        ) : isBinarySpreadsheet ? (
          <div className="attachment-preview-fallback">
            <AttachmentTypeIcon
              filename={preview.filename}
              mimeType={effectiveMime}
              bytes={bytes}
              size="lg"
            />
            <span>Inline preview is disabled for this spreadsheet format for security reasons.</span>
            <span className="attachment-preview-error">
              The xlsx parser carries unpatched high-severity advisories; download the file to open it locally.
            </span>
            <Button variant="primary" size="sm" onClick={onDownload}>
              Download
            </Button>
          </div>
        ) : spreadsheetPreview ? (
          sheet ? (
            <SpreadsheetPreview sheet={sheet} />
          ) : (
            <div className="attachment-preview-fallback">
              <AttachmentTypeIcon
                filename={preview.filename}
                mimeType={effectiveMime}
                bytes={bytes}
                size="lg"
              />
              <span>{sheetStatus || "Spreadsheet preview failed."}</span>
              {sheetError && <span className="attachment-preview-error">{sheetError}</span>}
              {sheetError && (
                <Button variant="primary" size="sm" onClick={onDownload}>
                  Download
                </Button>
              )}
            </div>
          )
        ) : textPreview != null ? (
          <pre className="attachment-text-preview">{textPreview}</pre>
        ) : (
          <div className="attachment-preview-fallback">
            <AttachmentTypeIcon
              filename={preview.filename}
              mimeType={effectiveMime}
              bytes={bytes}
              size="lg"
            />
            <span>Preview is not available for this file type yet.</span>
            <Button variant="primary" size="sm" onClick={onDownload}>
              Download
            </Button>
          </div>
        )}
      </div>
    </ModalShell>
  );
}
