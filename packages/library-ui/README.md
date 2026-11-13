# @rightkit/library-ui

Library list and reader over a host-supplied `LibrarySource` (the main entry holds no IPC); ScrapeRight queue screens on `./queue`, which do call Tauri.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/library-ui` | The library contract (`LibrarySource`, `LibraryRow`, `LibraryDetail`, `LibraryQuery`), host configuration (`configureLibraryHost`), the list (`LibraryScreen` and its parts, `useGroupSelection`, `OriginIcon`, list helpers) and the reader (`LibraryReader` and its parts, find helpers). |
| `@rightkit/library-ui/tauri` | `tauriLibrarySource()`: the `LibrarySource` over ScrapeRight's artifact library (`get_history_page`, `get_transcript`, `get_transcript_variant`, `delete_transcript`, the `job://update` event), and `LIBRARY_SORT_VARIANTS`. Imports Tauri. HeardRight's history is the `history` source of `@rightkit/dictation-ui/tauri`. |
| `@rightkit/library-ui/queue` | ScrapeRight's queue screens (paste bar, queue board, job cards, workbench preview, its Detail screen over the reader). Not IPC-free: this entry imports `@tauri-apps/api`, `@tauri-apps/plugin-dialog` and `@rightkit/tauri-shell` and calls ScrapeRight's Tauri commands by name; it stays here until a queue UI package exists. |
| `@rightkit/library-ui/styles.css` | Reader and queue styles, plus `@rightkit/doc-render`'s. |

## The contract

A ScrapeRight-shaped host writes `const source = tauriLibrarySource();` and passes it, with `variants={LIBRARY_SORT_VARIANTS}`
(the variant is the engine's sort order), to `LibraryScreen` and `LibraryReader`. Options: `commands` (command names),
`changeEvent`, `listen`, `copyText`. `update`, `exportAll` and `saveText` throw "not mapped" (ScrapeRight has no such
call); date and origin filters are not sent (the engine takes none).

The main entry (`@rightkit/library-ui`) holds no IPC; only `./queue` does (above). The host implements
`LibrarySource` over its own store and maps each record to a `LibraryRow` (list) and `LibraryDetail` (reader). Only `query` and `copyText` are required; the list and reader
hide what the source does not offer (`remove`, `update`, `exportAll`, `saveText`, `retention`, `recovery`,
`originIcon`, `detail`, `subscribe`).

```tsx
import { configureLibraryHost, LibraryScreen, LibraryReader } from "@rightkit/library-ui";

configureLibraryHost({ appName: "Your App", storageKeys: { skipDeleteConfirm: "your-app.skipDeleteConfirm" } });

<LibraryScreen source={source} search={query} variants={variants} groups={groups} labels={{ item: "note", items: "notes", area: "Notes" }} />
<LibraryReader source={source} id={selectedId} onBack={close} />
```

The origin filter shows each origin's `label` and sends its `filterKey` as `LibraryQuery.origin` (default: the
label). A source that matches on its own key (a raw process name, an id) sets `filterKey`; `deliveryRow` does.

Options of `LibraryScreen` for a host whose list must look and read as its own (every default is the package's own
output): `components` (`PageShell`: the screen frame, which then receives the toolbar as `header`; `ErrorNotice`: the
dismissible error line; `Button`: every button of the list), `classNames` (`input`: the text field of an edited row),
`originFilter` (`sortBy: "value"` sorts the origin options by the key sent to the source; `optionLabel` words an
option) and these `labels` beyond `item`, `items`, `area`, `alt` and `editHint`: `readOnly` (lock title and accessible
name), `expandTitle`, `deleteAllHint`, `groupGlyph`, `noMatch`, `deleteFailed` and `exportFields` (names of the origin
and text lines of "Export selected", and how the origin line's value is made). A row's `groupLabel` names its group
where the group's own label does not.

HeardRight's History page is the reference for the list (search, variants, date and origin filters, retention,
export, selection, edit, delete with confirmation, group visibility); ScrapeRight's Detail reader is the reference
for the reader (views, find, media, images, copy, save). `@rightkit/dictation-ui` maps dictation delivery records
to rows (`deliveryRow`) and wraps the list as `DictationHistory`.

## Host configuration

`configureLibraryHost({ appName?, storagePrefix?, storageKeys? })`: the product name in copy, and the browser-storage
keys the list keeps (`skipDeleteConfirm`, `groupSelection`, `recoveryDismissed`; default `rk-library.<key>`). A host
with stored choices passes its existing keys. The configuration is also importable alone from
`@rightkit/library-ui/host` (no list code), which is how `@rightkit/dictation-ui` forwards its own host configuration.

## Peer dependencies

`react`, `react-dom`

## Host contract

Hosts bundle this package with Vite. Tailwind hosts add `node_modules/@rightkit/library-ui/dist/**/*.js` to their
content globs and import `@rightkit/library-ui/styles.css` once.
