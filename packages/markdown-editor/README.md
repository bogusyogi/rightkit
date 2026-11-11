# @rightkit/markdown-editor

Markdown and MDX editor: ProseMirror WYSIWYG, source mode, math, diagrams and properties panel.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/markdown-editor` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/markdown-editor/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/markdown-editor/appName`. |
| `@rightkit/markdown-editor/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/document-core`

## Peer dependencies

`react`, `react-dom`

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). The store is the one
in `@rightkit/document-core/host`, which this package's `appName` module re-exports: call
`configureDocumentHost({ appName, id })` (or `setHostAppName("Your App")`) once at startup, before
rendering, and every document package reads it.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/markdown-editor/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/markdown-editor test
```

Modules left out of the root barrel because an export name collides with an earlier module
(import them by subpath): `documents/markdown/analysis/types.ts`, `documents/markdown/core/MarkdownDocumentTypes.ts`, `documents/markdown/core/MarkdownSourceMap.ts`, `documents/markdown/semanticBlocks.ts`, `documents/markdown/tables/markdownTableParser.ts`
