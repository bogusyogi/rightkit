# Changelog

## 0.1.6

Additive. Default `PdfViewer` output is intended to be unchanged. The characterization tests in `test/viewer-markup.test.tsx` pin only the initial rendered shell (no pages); the runtime `getDocument` call and text-layer render are not covered by tests.

- New optional `PdfViewer` prop `documentOptions`: pdf.js `getDocument` options passed beside the source (for example `wasmUrl`, `cMapUrl`, `cMapPacked`, `standardFontDataUrl`, `httpHeaders`). Read when loading starts, like `onPassword`. `url` and `data` cannot be overridden.
- `documentOptions` cannot replace the document source: `url`, `data` and `range` are dropped from it. `highlightSpan` ignores a `markBg`/`markActiveBg` value that could add a style declaration or fetch a resource. `renderPdfTextLayer` cannot be cancelled while rendering; a host that needs cancellation in `streamTextContent` mode builds the pdf.js `TextLayer` itself from `pdfTextContentSource`.
- New export `pdfDocumentInit(data, documentOptions?)` and type `PdfDocumentOptions`, `PdfDocumentInit`: the same init the viewer sends, for hosts that drive pdf.js themselves. With no options it is exactly `{ url }` or `{ data }`.
- New exports `attachPasswordHandler`, `createLoadTimer`, `MAX_PASSWORD_ATTEMPTS`, and types `PasswordTask`, `PasswordFlow`, `LoadTimer`: the password flow and load timer the viewer uses internally.
- New exports `pdfTextContentSource(page, mode?)` and `renderPdfTextLayer(page, container, viewport, mode?)`, with `PdfTextContentMode` (`"getTextContent"` default, or `"streamTextContent"`). The viewer calls `renderPdfTextLayer` with the default, so its output is unchanged.
- `highlightSpan(span, query, options)` gains `markBg`, `markActiveBg` and `activeOccurrence` (type `HighlightSpanOptions`). Without them the output is unchanged.
- Documentation fix: `types.ts` documented a `PdfViewer.onSearchProvider` prop that never existed. The docs now describe the working path (`findBar={false}` with `usePdfSearch()` in the `toolbar` slot). `PdfSearchProvider` stays exported as a type only.

## 0.1.5

- New optional `PdfViewer` prop `onPassword(reason, attempt)` for encrypted PDFs. `reason` is `"need"` (first request) or `"incorrect"` (last one was wrong). Return the string to retry with, or null/undefined to give up: the viewer shows no document and reports `PdfPasswordCancelledError` through `onError`. The load timeout is paused while the callback runs; a handler that keeps failing stops after 10 attempts. Absent: encrypted PDFs fail through `onError` exactly as before.
- New optional `PdfViewer` prop `maxRenderPages`. Pages past the cap stay as empty placeholders (`data-capped`) and are never rendered; thumbnails and find also stop at the cap. Default unlimited, so existing behaviour is unchanged.
- New exports: `PdfPasswordCancelledError`, `renderPageLimit`, types `PdfPasswordHandler`, `PdfPasswordReason`, `PdfViewerProps`. Backwards compatible.

## 0.1.4

- Republish with `dist/` present. 0.1.3 was published from an unbuilt tree and cannot be imported (exports point at ./dist/index.js); do not pin 0.1.3. `prepack` now builds.

## 0.1.3 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.1.2 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.1.1 — 2026-10-07

- BREAKING for `PdfViewer` users: `dist/PdfViewer.js` no longer imports `./styles.css` (CSS in a JS entry breaks Node ESM hosts). Import `@rightkit/pdf-ui/styles.css` once in the app. Helper-only consumers are unaffected.
