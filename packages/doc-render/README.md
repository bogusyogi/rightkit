# @rightkit/doc-render

Brand-neutral, read-only renderer for Markdown and sanitized HTML in Right Suite readers. `MarkdownRenderer` handles GFM, frontmatter, math, footnotes, citations, wiki links and safe links. `HtmlRenderer` sanitizes raw markup before it reaches React.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/doc-render` | `MarkdownRenderer`, `HtmlRenderer`, `renderMarkdown`, `renderHtml`, `parseMarkdown`, `parseToTree`, `sanitizeHtml`, `sanitizeMarkdownHtml`, `safeLinkHref`, `safeImageHref`, `isSafeUri`, `buildHeadingSlugs`, `collectFootnotes`, citation formatters, remark plugins, Shiki helpers |
| `@rightkit/doc-render/styles.css` | Stylesheet (import once) |

Peer dependency: `react` `>=18 <21`.

## Example

```tsx
import { MarkdownRenderer } from "@rightkit/doc-render";
import "@rightkit/doc-render/styles.css";

export function Doc({ markdown }: { markdown: string }) {
  return <MarkdownRenderer source={markdown} onOpenLink={(href) => console.log(href)} />;
}
```

`MarkdownRenderer` props: `source`, `className`, `onOpenLink`, `bibliography`.

## Known issues

- `mermaid` and `katex` are declared dependencies with no references in `src`, `test` or `styles`. They add about 111 packages to every install (production closure 236 down to 125, measured in the npm audit 2026-10-08). Removal is pending.
- `remark-math` parses math, but output is raw text in `<pre>`/`<code>` (`src/renderers.tsx:71-72`). Math is not rendered as KaTeX.
- Shiki loads its WASM through `shiki/onig.wasm?url`, so the host bundler must resolve `?url` imports.
