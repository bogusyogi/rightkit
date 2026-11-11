# @rightkit/library-ui

Library list and reader over a host-supplied `LibrarySource` (no IPC); ScrapeRight queue screens on `./queue`.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/library-ui` | The library contract (`LibrarySource`, `LibraryRow`, `LibraryDetail`, `LibraryQuery`), host configuration (`configureLibraryHost`), the list (`LibraryScreen` and its parts, `useGroupSelection`, `OriginIcon`, list helpers) and the reader (`LibraryReader` and its parts, find helpers). |
| `@rightkit/library-ui/queue` | ScrapeRight's queue screens (paste bar, queue board, job cards, workbench preview, its Detail screen over the reader). They still call ScrapeRight's Tauri commands by name; they stay here until a queue UI package exists. |
| `@rightkit/library-ui/styles.css` | Reader and queue styles, plus `@rightkit/doc-render`'s. |

## The contract

The package holds no IPC. The host implements `LibrarySource` over its own store and maps each record to a
`LibraryRow` (list) and `LibraryDetail` (reader). Only `query` and `copyText` are required; the list and reader
hide what the source does not offer (`remove`, `update`, `exportAll`, `saveText`, `retention`, `recovery`,
`originIcon`, `detail`, `subscribe`).

```tsx
import { configureLibraryHost, LibraryScreen, LibraryReader } from "@rightkit/library-ui";

configureLibraryHost({ appName: "Your App", storageKeys: { skipDeleteConfirm: "your-app.skipDeleteConfirm" } });

<LibraryScreen source={source} search={query} variants={variants} groups={groups} labels={{ item: "note", items: "notes", area: "Notes" }} />
<LibraryReader source={source} id={selectedId} onBack={close} />
```

HeardRight's History page is the reference for the list (search, variants, date and origin filters, retention,
export, selection, edit, delete with confirmation, group visibility); ScrapeRight's Detail reader is the reference
for the reader (views, find, media, images, copy, save). `@rightkit/dictation-ui` maps dictation delivery records
to rows (`deliveryRow`) and wraps the list as `DictationHistory`.

## Host configuration

`configureLibraryHost({ appName?, storagePrefix?, storageKeys? })`: the product name in copy, and the browser-storage
keys the list keeps (`skipDeleteConfirm`, `groupSelection`, `recoveryDismissed`; default `rk-library.<key>`). A host
with stored choices passes its existing keys.

## Peer dependencies

`react`, `react-dom`

## Host contract

Hosts bundle this package with Vite. Tailwind hosts add `node_modules/@rightkit/library-ui/dist/**/*.js` to their
content globs and import `@rightkit/library-ui/styles.css` once.
