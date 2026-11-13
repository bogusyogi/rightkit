# Changelog

## 0.2.3

- Added (optional, defaults unchanged): `LibraryOrigin.iconTitle`, the hover title of the origin icon tile (default `label`), and `LibraryOrigin.processName`, the raw process name a source uses for its icon lookup when it differs from `filterKey`.

## 0.2.2

Added: options so a host can make the list look and read as its own. Every default is unchanged: the rendered markup, strings, export text and storage of a host that sets none of them are byte-identical to 0.2.1.

- `LibraryScreen` props: `components` (`LibraryComponents`: `PageShell` (`LibraryPageShellProps`: `maxWidth`, `header`, `children`; the screen hands it the toolbar as `header` and the list in a full-width block, and draws no frame of its own), `ErrorNotice` (`LibraryErrorNoticeProps`), `Button` (`LibraryButtonProps`: `variant`, `size`, `disabled`, `onClick`, `buttonRef`, `children`; used by the row fix bar, the confirm dialog, Load more and the recovery banner)), `classNames` (`LibraryClassNames`: `input`, the edited row's text field) and `originFilter` (`LibraryOriginFilter`: `sortBy` `"label"` (default) or `"value"`, and `optionLabel(value, origin)`).
- `LibraryLabels` (all optional): `readOnly` (`{ title, label(rowContext) }`), `expandTitle(expanded)`, `deleteAllHint`, `groupGlyph(label)`, `noMatch`, `deleteFailed(failed, total)` and `exportFields` (`LibraryExportFields`: `origin` and `text` line names, `originValue(origin)`). `formatRowsPlainText` takes the fields as an optional third argument.
- `LibraryRow.groupLabel`: names a row's group where the group's label does not (the glyph's tooltip).
- `useLibraryButton()`: the list's button for parts a host renders itself.
- `./host` entry: `configureLibraryHost`, `libraryAppName`, `libraryStorageKey` and friends on their own, without the list code.

## 0.2.1

- Added: `./tauri` entry with `tauriLibrarySource(options?)`, the `LibrarySource` over ScrapeRight's artifact library (`get_history_page` by cursor behind the list's offsets, `get_transcript` and `get_transcript_variant` for the reader, `delete_transcript`, `job://update` for changes; command names overridable), and `LIBRARY_SORT_VARIANTS`.

- Fix: the origin filter never matched. Its options were the rows' display labels (a friendly app name such as "Google Chrome"), and that label went to the source as `LibraryQuery.origin`, while a source such as HeardRight's delivery store matches its own raw key (the process name). `LibraryOrigin` gains `filterKey` (the source's key, default `label`): the filter sends `filterKey` and shows `label`. `LibraryToolbar`'s `appOptions` also takes `{ value, label }` options (`LibraryFilterOption`); bare strings behave as before.
- Docs: "no IPC" holds for the main entry (contract, list, reader) only; the `./queue` entry imports `@tauri-apps/api`, `@tauri-apps/plugin-dialog` and `@rightkit/tauri-shell` and calls ScrapeRight's commands by name. README, CHANGELOG wording and the entry comments say so.

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-content-ingest-mail.md`; design in `reports/dictation-library-ui-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

The review judged this package the wrong shape: two apps' screens in two folders with no shared component, literal Tauri command names and one app's theme, licence and title bar. It is redrawn as one library contract with one list and one reader.

Added:
- `contract`: `LibrarySource` (the host's store: `query`, `copyText`, and optional `subscribe`, `remove`, `update`, `exportAll`, `saveText`, `retention`, `recovery`, `originIcon`, `detail`), `LibraryRow`, `LibraryOrigin`, `LibraryQuery`, `LibraryPageResult`, `LibraryVariant`, `LibraryGroup`, `LibraryDetail`. No IPC in the main entry (the `./queue` entry still imports Tauri; see 0.2.1).
- `host`: `configureLibraryHost({ appName, storagePrefix, storageKeys })`; storage keys are no longer `heardright.*` / `hr.*` literals.
- The list, from HeardRight's History page: `LibraryScreen` (filters, variants, retention, export, selection, edit, delete with confirmation, group visibility), `LibraryRows`, `LibraryRowView`, `LibraryToolbar`, `LibraryActionsLine`, `LibraryConfirmDialog`, `RecoveryBanner`, `OriginIcon`, `useLibraryQuery`, `useLibrarySelection`, `useGroupSelection`, list helpers (`format`). The generic half of `@rightkit/dictation-ui` 0.1.0's `pages/history/historyUtils` and `AppIcon` moved here.
- The reader, from ScrapeRight's Detail reader: `LibraryReader` over `source.detail(id)`, with the reader parts (`TranscriptBody`, `FindBar`, `CarouselLayout`, `ImageGrid`, `MediaPlayer`, `textForTab`) and find helpers on `LibraryDetail`.
- `./queue` entry for ScrapeRight's queue screens.

Removed (breaking):
- The dependency on `@rightkit/dictation-ui` (and on `@rightkit/license`, `@rightkit/app-update`, `@rightkit/settings-ui`, `@rightkit/theme`, `@tauri-apps/plugin-process`).
- ScrapeRight's app frame and its copies of owned code: `DesktopWorkspace`, `TitleBar`, `NavTabs` (use `@rightkit/react-shell`), `lib/theme` (`@rightkit/theme` and the app's theme), `lib/license` and `lib/updateAuth` (`@rightkit/license`, `@rightkit/app-update`), `views/settings/*` (`@rightkit/settings-ui` and the app), the QA mock `lib/ipc/qaMock` and `qaGate` (app QA code).
- ScrapeRight's library list `views/Library`, `LibraryList`, `LibraryToolbar` and `lib/historyPage`, `lib/desktopSources`: replaced by `LibraryScreen` over a `LibrarySource`.
- `appName` (`setHostAppName`, `hostAppVersion`): use `configureLibraryHost`.
- HeardRight's `Transcribe` page and `TranscribeModal` moved to `@rightkit/dictation-ui` (file transcription is dictation, not library).
- The `<path>` subpath exports: the entries are `.`, `./queue` and `./styles.css`.

Changed (breaking):
- `PasteBar` and `Detail` take `pro` (and `PasteBar` its starting `defaults`) as props instead of reading the removed licence and preference modules.
- "Export selected" writes `Origin:`/`Text:` lines (was `Target:`/`Transcript:`).
- `countWords` is one function (the reader's), shared by the queue preview.
- Stylesheets renamed: `queue/queue.css` (was `queue/components/ClaudeDesktop.css`), `reader/reader.css` (was `queue/views/ClaudeReader.css`).

Not changed here, recorded: the queue screens still call ScrapeRight's command names and stay on `./queue` until a queue UI package exists; the `claude-*` CSS class names are unchanged.

## 0.1.0

- First release. Library and queue UI for transcripts and downloads: queue board, job cards, library list, detail reader and history.
