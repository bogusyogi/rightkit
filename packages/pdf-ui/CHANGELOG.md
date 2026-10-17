# Changelog

## Unreleased

Fix and additive options (Orthic-Labs/rightkit#273, #396). Default output is unchanged.

- Viewer find (`PdfViewer`) no longer uses its own `toLocaleLowerCase` + `indexOf` loop, which had the #273 offset drift and no whitespace collapsing. It now calls `findLiteralMatches` per page, so counts and whole-word checks match `findLiteralMatches`.
- Regex find is denial-of-service bounded: `PdfViewer` runs it in a terminable Worker (blob URL, no bundler dependency) with a 250 ms per-page deadline; on expiry the Worker is terminated and `PdfSearchResult.timedOut` is true with partial results. New `createRegexSearcher(deadlineMs?)`, `MAX_REGEX_BOUND`, `REGEX_DEADLINE_MS`. Without `Worker`, matching is synchronous under a strict policy (`regexPatternError` default): at most 1 unbounded repeat (`MAX_REGEX_REPEATS` is now 1), no repeat on a group containing a repeat or alternation at any depth, repeat bounds at most 1000, so `(?:(a|aa))+b`, `a*a*b` and `a{0,100000}a{0,100000}b` are refused.
- New `findRegexMatches(text, pattern, { matchCase?, wholeWord? })` and `regexPatternError(pattern)`. Offsets index the text directly. A user pattern is refused (matches nothing) when it is longer than 200 characters, uses a backreference, repeats a group that already repeats or alternates (`(a+)+`, `(a|a)*`), has more than 2 unbounded repeats, or does not compile. Patterns are only compiled with `RegExp`, never evaluated. Only the first 100,000 units of text are searched per call. JavaScript has no step budget, so the bounds are structural.
- New optional `PdfViewer` prop `regexFind` (default false): shows a "Regex" checkbox in the find bar. `PdfSearchOptions` gains optional `regex`.
- `highlightSpan` takes `regex` (default false) in `HighlightSpanOptions`.
- New exports `findRegexMatches`, `regexPatternError`, `MAX_REGEX_PATTERN_LENGTH`, `MAX_REGEX_REPEATS`, `MAX_REGEX_TEXT`, type `RegexMatchOptions`. Tests in `test/regex.test.ts`; ß, ligature and whitespace cases added to `test/search.test.ts`.

## 0.1.8

Fix, plus additive option (Orthic-Labs/rightkit#273). 0.1.7 above was never published; the published version is 0.1.6.

- `findLiteralMatches` (and so `highlightSpan`) returned offsets that drifted after a character whose lower case has a different length (`"aİb".toLocaleLowerCase()` is 4 units, the source 3): every later match was shifted and the whole-word check read the wrong neighbours. Folding is now per character and each folded unit remembers its source character, so offsets always index the original text; a match never starts or ends inside a character that folds to two. Folding no longer depends on the locale (`toLowerCase`), and Greek final sigma folds with sigma. Behaviour change: results for text with such characters now point at the right place.
- Runs of whitespace (spaces, tabs, line breaks, no-break spaces) in the text and in the query each count as one space, so `"red apple"` finds `"red  apple"`; the match covers the whole run. A query that is only whitespace matches nothing; leading and trailing whitespace of the query is ignored.
- New option `joinHyphenated` (off by default): a hyphen between two letters that is followed by whitespace is ignored in the text, so `inter- national` matches `international`. New type exports `LiteralMatchOptions` and `TextMatch`. Tests in `test/search.test.ts`.

## 0.1.7

Additive. Default markup is unchanged and every colour keeps its previous value when no variable is set.

- Viewer surface colours are CSS variables with the old literals as fallbacks (#174): `--rk-pdf-ink`, `--rk-pdf-bg`, `--rk-pdf-chrome-bg` (toolbar, panels, notice), `--rk-pdf-page-bg`, `--rk-pdf-border`, `--rk-pdf-control-border`, `--rk-pdf-accent-bg`, `--rk-pdf-accent`, `--rk-pdf-error`, `--rk-pdf-muted`.
- New optional `PdfViewer` prop `truncationNotice` (a node, or a function of `{ shown, total }`): rendered as `<p class="rk-pdf-notice" role="status">` above the pages while `maxRenderPages` hides some, for a "showing first N of M pages" line. Absent: nothing is rendered. New export `PdfTruncationNotice`, the same slot for hosts that lay out their own page list.

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
