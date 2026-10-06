# @rightkit/pdf-ui

Read-only React PDF viewer backed by pdf.js. `PdfViewer` accepts bytes, a URL, or an async byte loader and provides bounded canvas rendering, text selection, outline/thumbnails, zoom, rotation, page navigation, search, and host-controlled safe external links.

Call `configurePdfJs` once when a host serves worker/wasm assets. `PdfViewer` never reads files itself and keeps document access in supplied `PdfSource` adapters.
