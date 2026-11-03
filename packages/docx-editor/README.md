# @rightkit/docx-editor

DOCX editing pane over @eigenpal/docx-editor with a host-owned toolbar, find, print and Markdown export.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/docx-editor` | Every module whose exported names do not collide (generated barrel). |
| `@rightkit/docx-editor/<path>` | Any module under `src/` by path without extension, e.g. `@rightkit/docx-editor/appName`. |
| `@rightkit/docx-editor/<path>.css` | Stylesheets shipped next to their components. |

## Depends on

`@rightkit/document-core`

## Peer dependencies

`react`

## Host-owned toolbar patch

The pane renders its own toolbar through `DocxEditor`'s `renderToolbar` prop and
`DocxHostToolbarContext` (typed in `documents/docx/eigenpalReact`). The published
`@eigenpal/docx-editor-react@1.9.0` does not have that prop yet: the source change is carried in
`patches/docx-editor-react-host-toolbar.patch` for an upstream pull request. Until it lands, the
workspace root applies it through `patchedDependencies` in `pnpm-workspace.yaml`, keyed
`@eigenpal/docx-editor-react@1.9.0` and pointing at `packages/docx-editor/patches/docx-editor-react-host-toolbar.patch`.
That root entry is not yet registered; until it is, a host must apply the patch itself. Against the
unpatched editor the prop is ignored and the editor shows its built-in toolbar.

`@rightkit/docx-editor/styles.css` imports the editor's own stylesheet, so a host imports this one
file and does not import the editor's stylesheet separately.

## Product name

User-facing copy reads the product name from `hostAppName()` (default `"App"`). Call
`setHostAppName("Your App")` (exported from the root and from `appName`) once at startup, before
rendering.

## Host contract

Hosts bundle this package with Vite (the build keeps extensionless relative imports and asset imports
such as `?raw`). Tailwind hosts add `node_modules/@rightkit/docx-editor/dist/**/*.js` to their content globs.

## Testing

```sh
pnpm --dir packages/docx-editor test
```
