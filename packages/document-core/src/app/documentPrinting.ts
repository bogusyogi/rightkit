import { mountPrintImage, printHost } from "./printPipelineShared";

const DOCUMENT_PRINT_CSS = `
  @page { margin: 0; }
  [data-vr-print-document] {
    box-sizing: border-box;
    width: 100%;
    height: auto;
    padding: 0.5in 0.68in;
    overflow: visible !important;
    background: #fff;
    color: #171717;
    font-family: var(--doc-font, "Source Serif 4", Georgia, serif);
    font-size: 10.5pt;
    line-height: 1.45;
    -webkit-box-decoration-break: clone;
    box-decoration-break: clone;
  }
  [data-vr-print-document] *, [data-vr-print-document] *::before, [data-vr-print-document] *::after {
    box-sizing: border-box;
  }
  [data-vr-print-document] :where(h1, h2, h3, h4, h5, h6) {
    break-after: avoid;
    page-break-after: avoid;
    color: #111;
    line-height: 1.2;
  }
  [data-vr-print-document] h1 { margin: 0 0 0.55em; font-size: 24pt; }
  [data-vr-print-document] h2 { margin: 1.35em 0 0.45em; font-size: 17pt; }
  [data-vr-print-document] h3 { margin: 1.15em 0 0.4em; font-size: 13pt; }
  [data-vr-print-document] :where(p, ul, ol, blockquote, pre, table) { margin-block: 0.55em 0.9em; }
  [data-vr-print-document] :where(pre, code) {
    font-family: var(--font-mono, "IBM Plex Mono", Consolas, monospace);
  }
  [data-vr-print-document] pre {
    max-width: 100%;
    overflow: visible !important;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    border: 1px solid #d8d2c7;
    border-radius: 5px;
    background: #f6f3ed;
    padding: 0.7em 0.8em;
    font-size: 9pt;
    line-height: 1.45;
  }
  [data-vr-print-document] :not(pre) > code {
    border: 1px solid #ddd6ca;
    border-radius: 3px;
    background: #f6f3ed;
    padding: 0.05em 0.28em;
    color: #60320e;
    font-size: 0.88em;
  }
  [data-vr-print-document] blockquote {
    margin-inline: 0;
    border: 1px solid #0c6aa5;
    border-radius: 4px;
    padding: 0.6em 0.9em;
    color: #444;
  }
  [data-vr-print-document] table {
    width: 100%;
    border-collapse: collapse;
    font-size: 9.5pt;
  }
  [data-vr-print-document] :where(th, td) {
    border: 1px solid #cfc8bc;
    padding: 0.38em 0.5em;
    text-align: left;
    vertical-align: top;
  }
  [data-vr-print-document] th { background: #f1eee8; }
  [data-vr-print-document] :where(img, svg, video, canvas) {
    max-width: 100% !important;
    height: auto !important;
  }
  [data-vr-print-document] a { color: #0c6aa5; text-decoration: underline; }
  [data-vr-print-document="plain-text"] pre {
    margin: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    padding: 0;
    color: #171717;
    font-size: 10pt;
    line-height: 1.5;
  }
`;

type PrintDocumentInput = { kind: "plain-text"; text: string };

/** Print pre-rendered HTML markup (e.g. a cloned DOCX pages element, or an iframe's serialized
 *  document) via the shared print host. The markup renders in the SAME document, so classes from
 *  already-loaded stylesheets (app theme, vendor DOCX CSS) still apply to the cloned nodes. */
export async function printHtmlMarkup(markup: string): Promise<boolean> {
  return printHost((host) => {
    host.innerHTML = markup;
  });
}

/** Print plain text from document state instead of the app's live editor tree: TXT uses its full
 *  model string because CodeMirror virtualizes off-screen lines and cannot be cloned whole. */
export async function printDocumentContent(input: PrintDocumentInput): Promise<boolean> {
  return printHost((host) => {
    const style = document.createElement("style");
    style.textContent = DOCUMENT_PRINT_CSS;
    host.appendChild(style);

    const sheet = document.createElement("article");
    sheet.setAttribute("data-vr-print-document", input.kind);
    const pre = document.createElement("pre");
    pre.textContent = input.text;
    sheet.appendChild(pre);
    host.appendChild(sheet);
  });
}

/** Print a serialized HTML document without allowing its styles to alter the host app shell.
 *
 * Interactive and safe HTML previews send their complete document back to the parent for printing.
 * That document can contain ordinary global CSS selectors, so mounting it directly in the app
 * document lets those selectors restyle the app behind macOS's print sheet. A shadow root keeps
 * both the document's authored CSS and print color settings inside this print-only subtree. */
export async function printIsolatedHtmlMarkup(markup: string): Promise<boolean> {
  return printHost((host) => {
    const pageStyle = document.createElement("style");
    pageStyle.textContent = "@page { margin: 0; }";
    host.appendChild(pageStyle);
    const root = host.attachShadow({ mode: "open" });
    root.innerHTML = `
      <style>
        :host {
          display: block;
          width: auto;
          height: auto;
          min-height: 0;
          padding: 0.5in 0.6in;
          overflow: visible !important;
          background: #fff;
          color-scheme: light;
          -webkit-box-decoration-break: clone;
          box-decoration-break: clone;
        }
        :host, *, *::before, *::after {
          box-sizing: border-box;
          -webkit-print-color-adjust: exact !important;
          print-color-adjust: exact !important;
        }
        html, body {
          position: static !important;
          width: auto !important;
          height: auto !important;
          min-height: 0 !important;
          overflow: visible !important;
        }
        body { margin: 0 !important; }
        img, svg, video, canvas { max-width: 100% !important; height: auto !important; }
        pre {
          max-width: 100%;
          overflow: visible !important;
          white-space: pre-wrap;
          overflow-wrap: anywhere;
        }
      </style>
      ${markup}`;
  });
}

/** Lazy bridge to the PDF print path. The pdf.js-dependent rasterizer lives in
 *  src/documents/pdf/pdfPrinting.ts so the shell's entry chunk never pays for it; this module keeps
 *  only the non-PDF printing helpers. The dynamic import resolves the SAME module the on-screen PDF
 *  viewer already loaded (it shares the one worker-src setup), so the worker URL is never resolved
 *  through a fresh chunk. See pdfPrinting.ts for the silent-hang trap this shape exists to avoid. */
async function lazyPdfPrinting(): Promise<typeof import("../documents/pdf/pdfPrinting")> {
  return import("../documents/pdf/pdfPrinting");
}

/** Print the PDF a viewer pane already has open, WITHOUT re-fetching it. See
 *  src/documents/pdf/pdfPrinting.ts. */
export async function printPdfDocument(
  doc: Parameters<typeof import("../documents/pdf/pdfPrinting").printPdfDocument>[0],
): Promise<boolean> {
  return lazyPdfPrinting().then((m) => m.printPdfDocument(doc));
}

/** Fallback PDF print path for when no viewer pane is registered: fetch the bytes from the stream URL
 *  and rasterize them. See src/documents/pdf/pdfPrinting.ts. */
export async function printPdf(assetUrl: string): Promise<boolean> {
  return lazyPdfPrinting().then((m) => m.printPdf(assetUrl));
}

/** Print a fully-baked image data: URI (crop/filters/text layers already rendered to a single canvas
 *  by exportNativeImageEdit — the same pipeline Save/Save As uses). Deliberately NOT the on-screen
 *  stage: that `<img>` loads through Tauri's custom asset:// scheme, which the native print
 *  pipeline's rendering pass does not reliably resolve (painted blank in testing, even though the
 *  same URL displays fine on screen), and cloning the transformed pan/zoom stage tree directly also
 *  came up blank in testing (likely a similarly WebKit-print-specific issue with re-styled clones). A
 *  single flat `<img src="data:...">` sidesteps both. */
export async function printImageDataUrl(dataUrl: string): Promise<boolean> {
  return printHost(async (host) => {
    await mountPrintImage(host, dataUrl, "display:block;width:100%;height:auto;");
  });
}
