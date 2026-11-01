import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import {
  attachmentTypeInfo,
  inferAttachmentMime,
  isInlinePreviewableSpreadsheet,
  isRenderableImageAttachment,
  isSpreadsheetAttachment,
} from "../src/lib/attachmentTypes";
import { MAX_PDF_PAGES, pdfPasswordCopy } from "../src/lib/attachmentPdf";

const PNG_BYTES = new Uint8Array([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
]);

test("attachment type inference treats weak MIME PNG files as renderable images", () => {
  assert.equal(
    inferAttachmentMime("邮件2.0.png", "application/octet-stream", PNG_BYTES),
    "image/png",
  );
  assert.equal(
    isRenderableImageAttachment("邮件2.0.png", "application/octet-stream", PNG_BYTES),
    true,
  );

  const info = attachmentTypeInfo("邮件2.0.png", "application/octet-stream", PNG_BYTES);
  assert.equal(info.label, "PNG");
  assert.equal(info.tone, "image");
  assert.equal(info.iconName, "image");
});

test("attachment type inference identifies spreadsheets by extension", () => {
  assert.equal(isSpreadsheetAttachment("orders_export_2026-06-02.xlsx", ""), true);

  const info = attachmentTypeInfo("orders_export_2026-06-02.xlsx", "");
  assert.equal(info.label, "XLSX");
  assert.equal(info.tone, "sheet");
});

test("inline preview is restricted to plain-text spreadsheet formats", () => {
  // CSV/TSV stay inline-previewable.
  assert.equal(isInlinePreviewableSpreadsheet("contacts.csv", ""), true);
  assert.equal(isInlinePreviewableSpreadsheet("data.tsv", ""), true);
  assert.equal(
    isInlinePreviewableSpreadsheet("report.csv", "text/csv"),
    true,
  );
  // Binary spreadsheet formats are recognised as spreadsheet attachments
  // (so the icon/chip is still right) but must NOT be inline-previewable.
  for (const name of [
    "orders.xlsx",
    "legacy.xls",
    "macro.xlsm",
    "binary.xlsb",
    "libre.ods",
  ]) {
    assert.equal(
      isSpreadsheetAttachment(name, ""),
      true,
      `${name} should still be a spreadsheet attachment`,
    );
    assert.equal(
      isInlinePreviewableSpreadsheet(name, ""),
      false,
      `${name} must not be inline-previewable while xlsx 0.18.5 advisories remain`,
    );
  }
});

test("PDF password prompt maps need to a prompt and incorrect to a re-prompt", () => {
  assert.equal(pdfPasswordCopy("need").title, "Password required");
  assert.equal(pdfPasswordCopy("incorrect").title, "Incorrect password");
  assert.match(pdfPasswordCopy("incorrect").body, /did not unlock/);
});

test("PDF render cap is 30 pages and passed to the viewer", () => {
  assert.equal(MAX_PDF_PAGES, 30);
  const component = readFileSync(join(process.cwd(), "src/components/AttachmentPdfPreview.tsx"), "utf8");
  assert.match(component, /maxRenderPages=\{MAX_PDF_PAGES\}/);
  assert.match(component, /onPassword=\{handlePassword\}/);
  assert.match(component, /truncationNotice=\{truncationNotice\}/);
});
