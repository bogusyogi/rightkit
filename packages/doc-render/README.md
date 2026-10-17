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

`MarkdownRenderer` props: `source`, `className`, `onOpenLink`, `bibliography`, `parseOptions`, `components`, `classNames`, `onText`, `linkPolicy`, `htmlPolicy`.

`components` overrides `link`, `text`, `inlineCode`, `code`, `table`, `heading`, `listItem`, `paragraph`, `blockquote`; an override returning `undefined` keeps the default element, and `link` is never called for a refused href. `onText(value, key)` returns a node to replace a text run or `undefined`.

`parseToTree(source, options)` options: `smartypants`, `frontmatter`, `citations`, `wikiLinks`, `defLists` (all default on; `false` disables), `softBreaks: "preserve"`, `emailAutolinks: false`, `highlight: true` (`==text==`, plain text only). Link nodes from bare autolinks carry `data.autolink`. The parse cache is capped at 8 MiB (`configureMarkdownParseCache`).

Sanitizer policy: `linkPolicy`/`htmlPolicy` add only `tel:` (`allowTel`), scheme-less relative references (`allowRelative`) and a relative-image `resolveImageSrc` hook, all off by default. The tag, attribute and CSS allowlists do not change. `renderMarkdownToHtml(source, options)` returns an HTML string under the same policies. `CodeBlock` ({ `code`, `language`, `onCopy`, `renderIcon` }) is a language-labelled copy-button block usable as `components.code`.

`HtmlRenderer` props: `html`, `className`, `safeMode` (default `true`; `safeMode={false}` renders the markup without sanitizing).

## Known issues

- `remark-math` parses math, but output is raw text in `<pre>`/`<code>` (`src/renderers.tsx:71-72`). Math is not rendered as KaTeX.
- Shiki, KaTeX and Mermaid-SVG sanitizer profiles and a full-document HTML sanitizer with styles (#395) are not provided.
- Shiki loads its WASM through `shiki/onig.wasm?url`, so the host bundler must resolve `?url` imports.
