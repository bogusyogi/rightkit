# @rightkit/pdf-ui

Brand-neutral, read-only PDF viewer for Right Suite apps, built on pdf.js. `PdfViewer` takes bytes, a URL or an async byte loader. It provides bounded canvas rendering, text selection, outline and thumbnails, zoom, rotation, page navigation and find. It does not mount a pdf.js annotation layer, so pages carry no clickable links; `createPdfLinkService` and `isSafeExternalUrl` are exported for a host that mounts its own, and only http(s) and mailto URLs reach the host's `openExternal`.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/pdf-ui` | `PdfViewer`, `PdfPasswordCancelledError`, `renderPageLimit`, `PdfSearchContext`, `usePdfSearch`, `createPdfLinkService`, `isSafeExternalUrl`, `configurePdfJs`, `ensurePdfWorker`, `pdfWasmUrl`, `acquirePdfRenderKeepalive`, `boundedCanvasDpr`, `clampScale`, `highlightSpan`, `viewportRotation`, `withTimeout`, `ZOOM_MIN`, `ZOOM_MAX`, `findLiteralMatches`, `thumbnailWindow`, `attachPasswordHandler`, `createLoadTimer`, `MAX_PASSWORD_ATTEMPTS`, `pdfDocumentInit`, `pdfTextContentSource`, `renderPdfTextLayer`; types |
| `@rightkit/pdf-ui/styles.css` | Stylesheet. Import once in the app. |

Dependency: `pdfjs-dist` `^6.3.289`.

Peer dependency: `react` `>=18 <21`.

## Example

```tsx
import { PdfViewer, configurePdfJs } from "@rightkit/pdf-ui";
import "@rightkit/pdf-ui/styles.css";

configurePdfJs({ workerSrc: "/pdfjs/pdf.worker.mjs" });

<PdfViewer source={{ kind: "url", url: "/docs/manual.pdf" }} documentOptions={{ wasmUrl: "/pdfjs/" }} onError={(e) => console.error(e)} />
```

Encrypted PDFs: pass `onPassword={(reason, attempt) => askUser(reason)}`. `reason` is `"need"` or `"incorrect"`. Return the string to retry with; return null/undefined to cancel (the viewer shows no document and calls `onError` with a `PdfPasswordCancelledError`). Without `onPassword`, encrypted PDFs fail through `onError`.

`maxRenderPages={n}` renders only the first `n` pages; the rest stay empty placeholders and thumbnails and find stop at the cap. Default: unlimited.

`PdfSource` is `{ kind: "bytes", data }`, `{ kind: "url", url }` or `{ kind: "loader", load() }`.

`documentOptions={{ wasmUrl, cMapUrl, standardFontDataUrl }}` passes pdf.js `getDocument` options beside the source. `url`, `data` and `range` cannot be overridden. It is read when loading starts; changing it does not reload an open document.

### Find UI owned by the host

Hide the built-in bar with `findBar={false}`, render your controls in the `toolbar` slot, and call `usePdfSearch()` inside them: `find(query?)`, `next()`, `prev()` and `result`. `PdfViewer` has no provider-registration prop.

### Headless pieces (build your own viewer)

- `pdfDocumentInit(data, documentOptions?)`: the `getDocument` init the viewer sends.
- `attachPasswordHandler(task, getHandler, timer?)` and `createLoadTimer(ms, message)`: the password flow and the load timer (its clock stops while the user is asked for a password).
- `pdfTextContentSource(page, mode?)` and `renderPdfTextLayer(page, container, viewport, mode?)`: the text layer, from `getTextContent` (default) or `streamTextContent` (`"streamTextContent"`).
- `highlightSpan(span, query, { matchCase, wholeWord, markBg, markActiveBg, activeOccurrence })`: find marks in a text-layer span. Without the colour options it uses the package colours.

## Known issues

- The host must serve the pdf.js worker and wasm files. `configurePdfJs({ workerSrc })` sets the worker URL; without it the worker resolves from `pdfjs-dist/build/pdf.worker.min.mjs` through `new URL(..., import.meta.url)`. `configurePdfJs({ wasmUrl })` only changes what `pdfWasmUrl()` returns (default `/pdfjs/`); `PdfViewer` does not read it, so pass `documentOptions={{ wasmUrl }}` to it. `PdfViewer` does not read files itself.
