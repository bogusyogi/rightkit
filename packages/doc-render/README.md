# @rightkit/doc-render

Brand-neutral, read-only renderer for Markdown and sanitized HTML in Right Suite readers. `MarkdownRenderer` handles GFM, frontmatter, math, footnotes, citations, wiki links and safe links. `HtmlRenderer` sanitizes raw markup before it reaches React.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see `package.json` (npm) |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/doc-render` | `MarkdownRenderer`, `HtmlRenderer`, `renderMarkdown`, `renderHtml`, `parseMarkdown`, `parseToTree`, `sanitizeHtml`, `sanitizeMarkdownHtml`, `safeLinkHref`, `safeImageHref`, `isSafeUri`, `buildHeadingSlugs`, `collectFootnotes`, citation formatters, remark plugins, Shiki helpers |
| `@rightkit/doc-render/styles.css` | Stylesheet (import once) |
| `@rightkit/doc-render/shiki.css` | Opt-in styles for `sanitizeShikiHtml` output |

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

## Shiki HTML profile

`sanitizeShikiHtml(input: string): string` accepts classic Shiki 4.5.0 output from
`codeToHtml(code, { lang, themes: { light: "github-light", dark: "github-dark" }, defaultColor: false })`.
Import `@rightkit/doc-render/shiki.css` once. Set `data-theme="dark"` on an ancestor
to switch themes. This additive API leaves `sanitizeHtml`, `sanitizeMarkdownHtml`,
`CodeBlock` & existing styles unchanged.

```tsx
import { sanitizeShikiHtml, ShikiSanitizationError } from "@rightkit/doc-render";
import "@rightkit/doc-render/shiki.css";

function Highlighted({ code, generatedHtml }: { code: string; generatedHtml: string }) {
  try {
    const html = sanitizeShikiHtml(generatedHtml);
    return <div dangerouslySetInnerHTML={{ __html: html }} />;
  } catch (error) {
    if (!(error instanceof ShikiSanitizationError)) throw error;
    return <>
      <p role="status">Syntax highlighting could not render; showing source text.</p>
      <pre><code>{code}</code></pre>
    </>;
  }
}
```

On refusal, render ORIGINAL `code` through React children or escaped text, never
rejected HTML. Keep source & copy actions bound to original code. Errors expose
fixed `code` values: `limit`, `syntax`, `structure`, `style`; no partial HTML escapes.
Unlike donor highlighting flow, this profile has finite caps; surface plain-source
fallback with notice for oversized documents as well as unsupported output.

Accepted markup is one HTML `pre` (`class`, `style`, `tabindex="0"`) containing
one attribute-free `code`, line spans (`class="line"`) separated by exact LF text,
& token spans with optional style & at most one text child. Empty lines/tokens are
accepted. Parser repairs, ignored source intervals, comments, other markup,
foreign namespaces & extra attributes fail closed. Entities remain code text.
Output discards theme-class metadata & uses fixed
`rk-shiki-profile shiki shiki-themes` classes.

Root style requires `--shiki-light`, `--shiki-dark`, `--shiki-light-bg`,
`--shiki-dark-bg`. Styled tokens require both theme colors & permit paired
`--shiki-{light,dark}-font-style` (`italic|inherit`), `font-weight` (`bold|inherit`),
& `text-decoration` (`underline|line-through|underline line-through|inherit`). Each
present font pair needs at least one non-`inherit` value. Colors accept only
case-insensitive `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`. Declarations use canonical
`key:value;key:value` syntax with no trailing semicolon. Ordinary CSS properties,
token backgrounds, functions, URLs, escapes, comments & duplicates are refused.
Opt-in CSS resets six font variables & root font properties before consuming
validated token values.

Caps: input 4,194,304 UTF-16 units & UTF-8 bytes; output 4,194,304 UTF-8 bytes;
65,536 allocated parser nodes including scaffold; parser stack depth 8; payload
element depth 4; 16,384 lines; class/style values 512 units each. Parsing uses
parse5 8.0.1 without browser DOM/network parsing, with scripting disabled,
allocation/depth guards & complete source-location partition checks. Serialization
uses typed records & fixed tags/keys with incremental escaping.

Before parse5, one forward lexical work guard caps raw tags at 2,048 UTF-16 units
& accepts only known tags with bounded, distinct, double-quoted attributes
(`pre`: at most 3; `span`: at most 1; `code`: none). Quoted `>` remains attribute
data. This preflight bounds tokenizer attribute work; parse5 & semantic/source
validation still decide acceptance.

Tests generate real fixtures with exact Shiki 4.5.0. Different producers, single
themes, custom transformers, font-only tokens without both colors, inline output,
token backgrounds & other unsupported forms require explicit text fallback.

## Known issues

- `remark-math` parses math, but output is raw text in `<pre>`/`<code>` (`src/renderers.tsx:71-72`). Math is not rendered as KaTeX.
- KaTeX & Mermaid-SVG profiles & full-document HTML sanitization with styles (#395) are not provided.
- Shiki loads its WASM through `shiki/onig.wasm?url`, so the host bundler must resolve `?url` imports.
