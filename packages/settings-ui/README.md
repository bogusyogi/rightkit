# @rightkit/settings-ui

Settings panel frame and standard sections for Right Suite apps: appearance, updates, models, license, legal, API keys, diagnostics, doctor and about. Apps keep their own design through props, slots and `--rk-*` tokens. The panel persists through an app-supplied backend. The host-side store is `rightkit-settings` (named in the package's backend docs).

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | `rightkit-settings` is the host persistence store (named in the backend docs). The app supplies the backend adapter. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/settings-ui` | Everything in `/core` and `/standard` (the 0.3.x surface, unchanged, plus the search exports) |
| `@rightkit/settings-ui/core` | `SettingsPanel`, `SectionFrame`, `mergeSections`, `orderSections`, `searchSections`, `revealSetting`, `SettingsGroup`, `SettingRow`, `ProgressBar`, `formatBytes`, `useSettings`, `createMemorySettings`, `sectionIcons`, the search API (`SettingsSearchProvider`, `SettingsItem`, `SettingsSearchSection`, `SettingsSectionScope`, `HighlightedText`, `useSettingsSearch`, `useSearchKeys`), and types. Imports only `react`. |
| `@rightkit/settings-ui/standard` | `standardSections`, the section factories, `useLicenseSection`, `useDoctor`. Needs the optional peers below. |
| `@rightkit/settings-ui/settings.css` | Stylesheet |
| `@rightkit/settings-ui/shell-adapter` | `createShellBackends` and the `rightkit-settings` / `rightkit-models` engine wire constants, over the `@rightkit/shell` engine bridge. The engine is described structurally, so this subpath needs no import of `@rightkit/shell`. |

The persistence port is the `SettingsBackend` interface (`load`, `patch` with `expectedRevision`, optional `subscribe`). Use the shell adapter in a Tauri app, or implement the interface over your own store. For example, a blob-backed adapter keeps the whole settings object in one JSON string, bumps a revision on each patch and throws `code: "revision_conflict"` on a stale write. `useSettings(backend)` takes any `SettingsBackend`. `createMemorySettings` is the in-memory reference implementation.

## Peer dependencies

| Peer | Range | Note |
|---|---|---|
| `@rightkit/app-shell` | `>=0.2.0 <1` | |
| `@rightkit/license` | `>=0.1.0` | Optional. Needed by `/standard` and the root entry only. No upper bound. |
| `@rightkit/logs` | `>=0.1.0` | Optional. Needed by `/standard` and the root entry only (type use). No upper bound. |
| `@rightkit/theme` | `>=0.2.0 <1` | Optional. Needed by `/standard` and the root entry only. |
| `@rightkit/updates` | `>=0.2.0` | Optional. Needed by `/standard` and the root entry only (type use). No upper bound. |
| `react` | `>=18 <21` | |

## Example

Illustrative. `backend` is the app's `SettingsBackend` adapter.

```tsx
import { SettingsPanel, standardSections } from "@rightkit/settings-ui";
import "@rightkit/settings-ui/settings.css";

const sections = standardSections();
<SettingsPanel backend={backend} sections={sections} />;
```

`standardSections` takes `about`, `appearance`, `updates`, `license`, `doctor` and other options. `useLicenseSection(options)` and `useDoctor(checks)` are headless hooks for apps that draw their own markup.

### Panel options for matching an existing settings panel

| Option | Default | Effect |
|---|---|---|
| `hideSectionHeading` (panel prop, or on a section) | `false` | Omits the section `<h1 class="rk-settings__h1">`. The article is labelled by the section `label`. A section value overrides the panel value. |
| `initial` (panel prop, or `useSettings(backend, { initial })`) | none | A `SettingsSnapshot` the app already holds, such as `createMemorySettings(...).snapshot()`. The first render is loaded, so "Loading settings…" never shows. |

### Using only the panel and the primitives

`@rightkit/settings-ui/core` is the panel shell, `SettingRow`, `SettingsGroup`, search and the backends. It imports `react` and nothing else, so an app that draws its own sections installs none of `@rightkit/license`, `logs`, `theme` or `updates` (they are optional peers). `@rightkit/app-shell` remains a peer for the stylesheet tokens (`settings.css` reads `--rk-*`); `/core` does not import it. The root entry still includes the standard sections, so importing from `@rightkit/settings-ui` needs all peers installed, as in 0.3.x. `src/core-entry.test.ts` walks the import graph of `/core` (source and `dist`) and fails if any of the four appears.

## Search

By default search behaves exactly as in 0.3.x: the nav lists the sections and `entries` that match, one section shows. Everything below is opt-in and renders nothing new unless you ask for it.

Rows, groups and items **register with a search context on mount and unregister on unmount**. What is searchable is therefore what is rendered: a row the backend does not publish a key for is never a result, a row that loads late is a result the moment it mounts, and no `MutationObserver` or second `entries` list is needed.

```tsx
import { SettingsPanel, SettingsGroup, SettingRow, SettingsItem } from "@rightkit/settings-ui/core";
import "@rightkit/settings-ui/settings.css";

const sections = [
  {
    id: "general", label: "General", group: "Pulse",
    render: (ctx) => (
      <SettingsGroup title="Startup" hideWhenEmpty>
        <SettingRow label="Open at login" description="Start Pulse when you sign in." keywords={["autostart"]}>
          <Toggle checked={ctx.values.login === true} onChange={(v) => ctx.set("login", v)} />
        </SettingRow>
        {published("hotkey") ? <SettingRow label="Global hotkey" description="Opens the launcher.">{/* ... */}</SettingRow> : null}
      </SettingsGroup>
    ),
  },
  {
    id: "accounts", label: "Accounts",
    render: () => (
      <SettingsGroup title="Signed in" hideWhenEmpty>
        <SettingsItem text="Connected accounts mail calendar"><AccountList /></SettingsItem>
      </SettingsGroup>
    ),
  },
];

// No backend, controlled query and section, all-sections results, Pulse's keyboard, marked matches.
<SettingsPanel
  sections={sections}
  searchMode="all-sections"
  query={query} onQueryChange={setQuery}
  activeSection={section} onActiveSectionChange={setSection}
  highlight
  findShortcut
  escapeClears="unless-in-input-or-dialog"
  openSectionLabel={(title) => `Open ${title}`}
/>
```

If the host draws its own search field and layout, use the context directly (this is the shape of Pulse's `settingsKit.tsx`):

```tsx
<SettingsSearchProvider query={query.trim()} highlight>
  <input ref={input} value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search settings" />
  {sections.map((s) => (
    <SettingsSearchSection key={s.id} id={s.id} title={s.title} onOpen={open}>{s.body}</SettingsSearchSection>
  ))}
</SettingsSearchProvider>
```

with `useSearchKeys({ inputRef: input, query, setQuery, findShortcut: true, escapeClears: "unless-in-input-or-dialog" })` for the keyboard and `useSettingsSearch()` for `{ results, count, active }`.

### Matching

The query is split on whitespace and every term must appear (case-insensitive) in a row's label, description, `keywords`, its group title (string titles), its section title, or the section's `keywords` and nav `group`. A row whose `label` is not a string needs `searchText`. A group whose title matches keeps all its rows. Content that is not a `SettingRow` takes part through `SettingsItem text=…` and is shown only while it matches.

### Options

| Where | Option | Default | Effect |
|---|---|---|---|
| `SettingsPanel` | `searchMode` | `"navigate"` | `"navigate"`: today's nav and hit buttons, rows are never hidden. `"all-sections"`: while a query is active every section renders its matching rows and items with live controls, under a heading (default `h2`, `resultHeadingLevel`) and an "Open section" button (`openSectionLabel`, a string or `(title) => text`); sections with no match hide; the nav stays whole; clearing the query restores the active section and the body scroll. Opening a section clears the query and starts at the top. |
| `SettingsPanel` | `backend` | optional | Without it the panel keeps an in-memory store, so `ctx.set` works and nothing flashes "Loading settings…". |
| `SettingsPanel` | `query`, `onQueryChange` | uncontrolled | Controlled query. Works with `hideNav` and `hideSearch`, with the host owning the input. |
| `SettingsPanel` | `activeSection`, `onActiveSectionChange` | none | Aliases of `activeId` / `onActiveChange`; the new name wins when both are given, and both callbacks fire. |
| `SettingsPanel` | `hideNav` | `false` | Omits the nav. As in 0.3.x it also leaves out the search field **unless `search` is `true` explicitly**, in which case the field sits above the content in a `role="search"` wrapper. (Compatibility: `search` defaults to "on with the nav", so an existing `hideNav` app gets no new field.) |
| `SettingsPanel` | `hideSearch` | `false` | Never render the built-in field. Overrides `search`. `search={false}` still works. |
| `SettingsPanel` | `searchLabel` | `"Search settings"` | Accessible name and placeholder of the field. |
| `SettingsPanel`, `SettingsSearchProvider` | `highlight` | off | `true` wraps matches in `<mark class="rk-settings__mark">`; a function renders each matched piece. Matches are marked in row labels and descriptions, group titles and result headings; text content and accessible names are unchanged. |
| `SettingsPanel` | `findShortcut` | `false` | `true`: Cmd+F (Ctrl+F off macOS) focuses and selects the field and cancels the browser's find. A function decides for itself. |
| `SettingsPanel` | `escapeClears` | none | `"always"`: Escape clears from anywhere. `"when-search-focused"`: only from the field. `"unless-in-input-or-dialog"`: from the field or the page, but not from another input, textarea, select, contenteditable, `dialog` or `[role=dialog]`. Ignored during IME composition and when another handler already called `preventDefault`. |
| `SettingsPanel` | `announceResults`, `formatAnnouncement` | on in all-sections, off in navigate | A polite `role="status"` live region (`rk-settings__sr`) with the count ("3 settings found", "No settings found"), updated 300 ms after typing settles. Off by default in navigate mode so default markup is unchanged. |
| `SettingsGroup` | `hideWhenEmpty` | `false` | Under a provider, hide the group while none of its registered rows or items is visible; re-evaluated on every registration change, so late rows reveal it. Wrap non-row content in `SettingsItem` to count. |
| `SettingRow` | `keywords`, `searchText` | none | Extra terms; text to match when `label` is not a string. |

Focus is never moved by search: typing, changing results and opening results leave focus where it was (only the find shortcut and an explicit "Open section" click act on focus).

Rows that do not match are unmounted while a query is active in all-sections mode, so a draft typed into a control that stops matching is lost; keep drafts in state the app owns if that matters.

### Pulse's seven requirements

| # | Pulse requirement (gap row 104) | API |
|---|---|---|
| 1 | Rows and groups take part in search themselves; no second `entries` list | `SettingRow`, `SettingsGroup`, `SettingsItem` register with `SettingsSearchProvider` (the panel provides it); label, `description`, `keywords`, group title and section title are matched |
| 2 | All-sections results with live controls, grouped by section, "Open" action | `searchMode="all-sections"`, `openSectionLabel`, `resultHeadingLevel`, or `SettingsSearchSection` directly |
| 3 | Items that are not rows (accounts, permissions, launcher lists) are searchable | `<SettingsItem text=… keywords=…>` |
| 4 | Rows whose key the backend does not publish are not results; late rows appear | Registration follows mount; render the row conditionally. `hideWhenEmpty` on groups re-evaluates on registration |
| 5 | Panel without a `SettingsBackend`; `hideNav` must not disable search; no required license/logs/theme/updates peers | optional `backend`, `query`/`onQueryChange`, `activeSection`/`onActiveSectionChange`, `hideSearch`, `search` with `hideNav`; `@rightkit/settings-ui/core` and optional peers |
| 6 | Find shortcut and scoped Escape | `findShortcut`, `escapeClears="unless-in-input-or-dialog"`, or the `useSearchKeys` hook for a host-owned field |
| 7 | Late-loading rows re-check hidden groups and sections | Same registration mechanism, no `MutationObserver`; `SettingsGroup hideWhenEmpty`, `SettingsSearchSection hideWhenEmpty` (default true) |

Differences from Pulse's local kit, so nothing surprises you on adoption: matching is "every whitespace-separated term" where Pulse matches the whole trimmed string as one substring (a superset of its results); hiding uses `display: none` on groups and sections but unmounts non-matching rows and items (Pulse also returns `null` for them); the section action is a `<button>` whose text is yours via `openSectionLabel`.

### Tokens

Set on `:root` or `.rk-settings`. Every default is today's built-in value.

| Token | Default | Controls |
|---|---|---|
| `--rk-settings-padding` | `24px 32px 40px` | Body padding. A value set here applies at every width. Unset, the narrow screen (640px and under) uses `18px 16px 32px`. |
| `--rk-settings-font-size` | `13.5px` | Base font size of the panel |

The panel reads the suite colour tokens from `@rightkit/app-shell/shell.css` (`--rk-plane`, `--rk-chrome`, `--rk-ink` and the rest). It brings no colours of its own.

## Known issues

- Three peers have no upper bound: `@rightkit/license`, `@rightkit/logs` and `@rightkit/updates` (npm audit 2026-10-08, settings-ui finding).
- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:38`).
