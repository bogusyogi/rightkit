# @rightkit/pdf-ui

Brand-neutral, read-only PDF viewer for Right Suite apps, built on pdf.js. `PdfViewer` takes bytes, a URL or an async byte loader. It provides bounded canvas rendering, text selection, outline and thumbnails, zoom, rotation, page navigation, find, and host-controlled external links.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/pdf-ui` | `PdfViewer`, `PdfSearchContext`, `usePdfSearch`, `createPdfLinkService`, `isSafeExternalUrl`, `configurePdfJs`, `ensurePdfWorker`, `pdfWasmUrl`, `acquirePdfRenderKeepalive`, `boundedCanvasDpr`, `clampScale`, `highlightSpan`, `viewportRotation`, `withTimeout`, `ZOOM_MIN`, `ZOOM_MAX`, `findLiteralMatches`, `thumbnailWindow`; types |
| `@rightkit/pdf-ui/styles.css` | Stylesheet. Import once in the app. |

Dependency: `pdfjs-dist` `^6.3.289`.

Peer dependency: `react` `>=18 <21`.

## Example

```tsx
import { PdfViewer, configurePdfJs } from "@rightkit/pdf-ui";
import "@rightkit/pdf-ui/styles.css";

configurePdfJs({ workerSrc: "/pdfjs/pdf.worker.mjs", wasmUrl: "/pdfjs/" });

<PdfViewer source={{ kind: "url", url: "/docs/manual.pdf" }} onError={(e) => console.error(e)} />
```

`PdfSource` is `{ kind: "bytes", data }`, `{ kind: "url", url }` or `{ kind: "loader", load() }`.

## Known issues

- The host must serve the pdf.js worker and wasm files. Call `configurePdfJs` with their URLs (`workerSrc`, `wasmUrl`). `PdfViewer` does not read files itself.
