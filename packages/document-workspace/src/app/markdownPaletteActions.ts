import { toast } from "@rightkit/platform-ui/headless";
import { isMarkdownFormat } from "../documents/capabilities.js";
import { baseName } from "@rightkit/document-core/documents/types";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { MarkdownExportApi } from "@rightkit/markdown-editor/documents/markdown/export/markdownExportTypes";
import { saveFileDialog, writeDocument } from "@rightkit/document-core/state/tauriBridge";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";

const isMarkdownLike = (active: LoadedDocument | null): active is LoadedDocument =>
  !!active && isMarkdownFormat(active?.format);

/** Command-palette "Export as…" action for md/mdx, or null for other formats. */
export function buildExportMarkdown(
  active: LoadedDocument | null,
  markdownExport: MarkdownExportApi | null,
  activeText: string,
): ((target: "html" | "txt") => void) | null {
  if (!isMarkdownLike(active)) return null;
  return (target: "html" | "txt") => {
    const stem = baseName(active.path).replace(/\.(md|mdx|markdown)$/i, "");
    if (active.format === "md") {
      if (markdownExport?.documentPath !== active.path) {
        toast("Markdown renderer is still loading", "info");
        return;
      }
      void markdownExport
        .createSnapshot()
        .then(async (snapshot) => {
          const path = await saveFileDialog(
            `${stem}.${target === "html" ? "html" : "txt"}`,
            target === "html" ? "html" : "txt",
          );
          if (!path) return;
          await writeDocument(path, target === "html" ? snapshot.htmlDocument : snapshot.plainText);
          toast(
            snapshot.warnings.length
              ? `Exported ${baseName(path)} with ${snapshot.warnings.length} warning${snapshot.warnings.length === 1 ? "" : "s"}`
              : `Exported ${baseName(path)}`,
            snapshot.warnings.length ? "info" : "success",
          );
        })
        .catch((error) => toast(`Export failed: ${errorMessage(error)}`, "error"));
      return;
    }
    // Lazy: exportMarkdown pulls the whole unified/remark toolchain. Only pay for it once the
    // user actually asks to export MDX, whose sandbox renderer is separate from Markdown.
    void import("@rightkit/markdown-editor/documents/markdown/exportMarkdown").then(
      ({ markdownToHtml, markdownToPlainText }) => {
        const output =
          target === "html" ? markdownToHtml(activeText, stem) : markdownToPlainText(activeText);
        void saveFileDialog(
          `${stem}.${target === "html" ? "html" : "txt"}`,
          target === "html" ? "html" : "txt",
        ).then((path) => {
          if (!path) return;
          writeDocument(path, output)
            .then(() => toast(`Exported ${baseName(path)}`, "success"))
            .catch((e) => toast(`Export failed: ${errorMessage(e)}`, "error"));
        });
      },
    );
  };
}

/** Command-palette "Copy rendered" action for md/mdx, or null for other formats. */
export function buildCopyRendered(
  active: LoadedDocument | null,
  markdownExport: MarkdownExportApi | null,
  activeText: string,
): (() => void) | null {
  if (!isMarkdownLike(active)) return null;
  return () => {
    const copy = (html: string, text: string) => {
      const clip = navigator.clipboard;
      const ok = () => toast("Rendered Markdown copied", "success");
      const fail = () => toast("Couldn't copy to clipboard", "error");
      if (clip?.write && typeof ClipboardItem !== "undefined") {
        void clip
          .write([
            new ClipboardItem({
              "text/html": new Blob([html], { type: "text/html" }),
              "text/plain": new Blob([text], { type: "text/plain" }),
            }),
          ])
          .then(ok)
          .catch(() => void clip.writeText(text).then(ok).catch(fail));
      } else {
        void clip?.writeText(text).then(ok).catch(fail);
      }
    };
    if (active.format === "md") {
      if (markdownExport?.documentPath !== active.path) {
        toast("Markdown renderer is still loading", "info");
        return;
      }
      void markdownExport
        .createSnapshot()
        .then((snapshot) => copy(snapshot.htmlFragment, snapshot.plainText))
        .catch(() => toast("Couldn't copy to clipboard", "error"));
      return;
    }
    void import("@rightkit/markdown-editor/documents/markdown/exportMarkdown").then(
      ({ markdownToHtmlFragment, markdownToPlainText }) => {
        copy(markdownToHtmlFragment(activeText), markdownToPlainText(activeText));
      },
    );
  };
}

/** Command-palette "Copy as Markdown" for PDF and DOCX (converted from document text/model). */
export function buildCopyAsMarkdown(active: LoadedDocument | null): (() => void) | null {
  if (!active || (active.format !== "pdf" && active.format !== "docx")) return null;
  const { format, assetUrl, content } = active;
  // DOCX without an asset URL carries its bytes as base64 content (DocumentRouter's initialBase64).
  if (!assetUrl && (format === "pdf" || !content)) return null;
  return () => {
    const convert =
      format === "pdf"
        ? import("@rightkit/document-core/documents/pdf/pdfToMarkdown").then((m) => m.pdfUrlToMarkdown(assetUrl as string))
        : (assetUrl
            ? fetch(assetUrl).then((r) => r.arrayBuffer())
            : import("@rightkit/docx-editor/documents/docx/blankDocx").then((m) => m.base64ToBuffer(content)))
            .then((buf) => import("@rightkit/docx-editor/documents/docx/docxToMarkdown").then((m) => m.docxBytesToMarkdown(buf)));
    void convert
      .then((md) => writeClipboardText(md))
      .then(() => toast("Markdown copied", "success"))
      .catch((e) => toast(`Couldn't copy as Markdown: ${errorMessage(e)}`, "error"));
  };
}
