# Changelog

## 0.2.1 — unpublished staging

- Add `sanitizeShikiHtml` & fixed-code `ShikiSanitizationError` for classic Shiki 4.5 dual-theme HTML, with guarded parse5 8.0.1 parsing, strict source/structure/CSS validation & bounded fixed-tag serialization.
- Add opt-in `@rightkit/doc-render/shiki.css` with scoped theme colors & font resets. Existing sanitizer/CSS defaults & source/copy behavior remain unchanged.
- Document original-source text fallback with highlighting-failed notice for unsupported output & finite limits; add real-producer fixture & adversarial test sources.

## 0.2.0 (2026-10-17)

- Sanitizer policy hooks (#395): `safeLinkHref(href, { allowTel, allowRelative })` and `sanitizeHtml`/`sanitizeMarkdownHtml(html, { allowTel, allowRelative, resolveImageSrc })`. Both link additions default off; tag, attribute and CSS allowlists are unchanged, and `javascript:`, `data:`, `srcdoc`, event handlers and styles stay refused. `resolveImageSrc` runs on relative image sources only; its return value is host-trusted.
- `==highlight==` support (`highlight: true` parse option, `remarkHighlight`, rendered as `<mark>`), 8 MiB byte-capped parse cache (`configureMarkdownParseCache`, `getMarkdownParseCacheBytes`), and `renderMarkdownToHtml` (HTML string with the same policies) (#395).
- `MarkdownRenderer` `components`, `classNames`, `onText`, `linkPolicy`, `htmlPolicy`, `parseOptions` props (#631).
- `parseToTree(source, options)`: `smartypants`, `frontmatter`, `citations`, `wikiLinks`, `defLists`, `softBreaks: "preserve"`, `emailAutolinks`, `highlight`; bare autolinks carry `data.autolink` (#632). Defaults are unchanged. The cache key includes the options.
- `CodeBlock` with language label and copy button (#633).
- Not shipped under #395: Shiki/KaTeX/Mermaid-SVG sanitizer profiles and the full-document visual sanitizer with `<style>` (would admit new tags, attributes or CSS properties the issue does not enumerate).

## 0.1.6

- Republish with `dist/` present. 0.1.5 was published from an unbuilt tree and cannot be imported (exports point at ./dist/index.js); do not pin 0.1.5. `prepack` now builds.

## 0.1.5 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.1.4 — 2026-10-09

- Removed the unused `mermaid` and `katex` dependencies (no references in `src`, `test` or `styles`). No runtime change; the production dependency closure shrinks for every consumer.

## 0.1.3 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.1.2 — 2026-10-07

- Every relative import in `dist` carries its `.js` extension (compiled with NodeNext), so the package loads under plain Node ESM (e.g. Vitest without inlining).
- The Shiki Oniguruma WASM URL (`shiki/onig.wasm?url`) is imported lazily on first highlighter build, not at module load; bundlers still emit the asset. `configureShikiWasm(url)` lets a host serve the WASM itself.

## 0.1.1 — 2026-10-07

- `buildBlocks(nodes, { hintPrefixes })` limits which `<!-- PREFIX: KIND -->` comments count as explicit hints (default `["doc", "viewright"]`). ViewRight passes `["viewright"]` for exact parity.
