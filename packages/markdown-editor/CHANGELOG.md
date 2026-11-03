# Changelog

## 0.1.1

- Ask 2: adds `markdownReading.css` (reading column typography and measure: `.reading`, `.vr-markdown-document-geometry`, `.vr-reading-measure`, `.vr-reading-container`, breakout rules and the `.vr-pm-markdown-doc` base). Driven by the host's `--doc-font`, `--doc-size`, `--doc-leading`, `--doc-fg` and `--doc-measure` (set by `@rightkit/document-core` from Settings). New export `./documents/markdown/markdownReading.css`, and it is imported from `styles.css`.
- Ask 3: the projection cache (`markdownToPmDoc`) is now a 24 MiB byte-bounded LRU instead of a 4-entry map. Entries are charged an estimate per source unit; documents over the budget are projected but not cached.
- Ask 4: pins `prosemirror-view` 1.42.8 and `prosemirror-transform` 1.12.4, matching the host, so the editor and host share one copy of each (plugin keys survive).

## 0.1.0

- First release. Markdown and MDX editor: ProseMirror WYSIWYG, source mode, math, diagrams and properties panel.
