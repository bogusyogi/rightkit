import { useCallback, useState } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { MarkdownExportApi } from "@rightkit/markdown-editor/documents/markdown/export/markdownExportTypes";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { printHtmlMarkup } from "@rightkit/document-core/app/documentPrinting";

/** Single-active-slot APIs the mounted format panes register (DOCX menu, PDF print, Markdown export)
 * plus the print router that prefers them. Extracted verbatim from App.tsx. */
export function useFormatApis(onPrint: (doc: LoadedDocument | null) => void) {
  const [docxMenu, setDocxMenu] = useState<DocxMenuApi | null>(null);
  // The active PDF pane's own print function, using the PDFDocumentProxy it already has loaded/
  // rendering on screen — NOT a fresh fetch of the same <host id>-stream:// URL, which hung
  // indefinitely with no error (see documents/pdf/pdfPrinting.ts printPdf's fallback for the no-registration
  // case). Mirrors the docxMenu single-active-slot pattern above.
  const [pdfPrint, setPdfPrint] = useState<(() => Promise<boolean>) | null>(null);
  const [markdownExport, setMarkdownExport] = useState<MarkdownExportApi | null>(null);
  // MUST wrap in an extra arrow: React useState setters treat a bare function argument as an updater
  // `(prev) => next`, so `setPdfPrint(fn)` would INVOKE the print function at registration time and
  // store its Promise instead of the function. That made every later `pdfPrint()` call throw
  // synchronously — no dialog, no toast, no timeout, which is exactly how PDF print "did nothing".
  const registerPdfPrint = useCallback(
    (fn: (() => Promise<boolean>) | null) => setPdfPrint(() => fn),
    [],
  );
  const registerMarkdownExport = useCallback(
    (api: MarkdownExportApi | null) => setMarkdownExport(api),
    [],
  );
  // Route PDF print through the active pane's registered function when available; every other
  // format keeps using useDocuments' onPrint unchanged.
  const handlePrint = useCallback(
    (doc: LoadedDocument | null) => {
      if (doc?.format === "pdf" && pdfPrint) {
        void pdfPrint()
          .then((printed) => {
            if (printed) return;
            logError("print:failed:pdf");
            toast("Couldn't open print dialog", "error");
          })
          .catch(() => {
            logError("print:failed:pdf");
            toast("Couldn't open print dialog", "error");
          });
        return;
      }
      if (doc?.format === "md" && markdownExport?.documentPath === doc.path) {
        void markdownExport
          .createSnapshot()
          .then(async (snapshot) => {
            const printed = await printHtmlMarkup(snapshot.htmlFragment);
            if (!printed) throw new Error("Print dialog did not open");
            if (snapshot.warnings.length) {
              toast(
                `Printed with ${snapshot.warnings.length} warning${snapshot.warnings.length === 1 ? "" : "s"}`,
                "info",
              );
            }
          })
          .catch(() => {
            logError("print:failed:md");
            toast("Couldn't open print dialog", "error");
          });
        return;
      }
      if (doc?.format === "md") {
        toast("Markdown renderer is still loading", "info");
        return;
      }
      onPrint(doc);
    },
    [markdownExport, pdfPrint, onPrint],
  );
  return { docxMenu, setDocxMenu, markdownExport, registerPdfPrint, registerMarkdownExport, handlePrint };
}
