# @rightkit/settings-ui

Settings panel frame and standard sections for Right Suite apps: appearance, updates, models, license, legal, API keys, diagnostics, doctor and about. Apps keep their own design through props, slots and `--rk-*` tokens. The panel persists through an app-supplied backend. The host-side store is `rightkit-app-settings` (named in the package's backend docs).

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | `rightkit-app-settings` is the host persistence store (named in the backend docs). The app supplies the backend adapter. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/settings-ui` | Everything in `/core` and `/standard` (the 0.3.x surface, unchanged, plus the search exports) |
| `@rightkit/settings-ui/core` | `SettingsPanel`, `SectionFrame`, `mergeSections`, `orderSections`, `searchSections`, `revealSetting`, `SettingsGroup`, `SettingRow`, `ProgressBar`, `formatBytes`, `useSettings`, `createMemorySettings`, `sectionIcons`, the search API (`SettingsSearchProvider`, `SettingsItem`, `SettingsSearchSection`, `SettingsSectionScope`, `HighlightedText`, `useSettingsSearch`, `useSearchKeys`), and types. Imports only `react`. |
| `@rightkit/settings-ui/standard` | `standardSections`, the section factories, `useLicenseSection`, `useDoctor`. Needs the optional peers below. |
| `@rightkit/settings-ui/headless` | The search state as hooks and no-DOM providers, for a host with its own markup: `SettingsSearchProvider`, `useSettingsSearch`, `useSettingsRow`, `useSettingsItem`, `useSettingsHit`, `useSettingsGroup`, `useSettingsSection`, `useSettingsSearchField`, `useSettingsSearchStatus`, `useSearchKeys`, `highlightText`, `parseSearchTerms`, `defaultAnnouncement`, `SettingsGroupScope`, `SettingsSectionScope`. Imports only `react`; no stylesheet. See [Headless search](#headless-search). |
| `@rightkit/settings-ui/settings.css` | Stylesheet |
| `@rightkit/settings-ui/shell-adapter` | `createShellBackends` (settings, models and secrets backends) and the engine wire constants `METHODS` and `EVENTS` for `rightkit-app-settings` / `rightkit-models`, over the `@rightkit/tauri-shell` engine bridge. The engine is described structurally, so this subpath needs no import of `@rightkit/tauri-shell`. |

The persistence port is the `SettingsBackend` interface (`load`, `patch` with `expectedRevision`, optional `subscribe`). Use the shell adapter in a Tauri app, or implement the interface over your own store. For example, a blob-backed adapter keeps the whole settings object in one JSON string, bumps a revision on each patch and throws `code: "revision_conflict"` on a stale write. `useSettings(backend)` takes any `SettingsBackend`. `createMemorySettings` is the in-memory reference implementation. The root and `/core` also export `createMemorySecrets` (an in-memory `SecretsBackend`), `RevisionConflict`, and `createShellBackends`, `METHODS` and `EVENTS` as deprecated re-exports of `/shell-adapter`.

## Peer dependencies

| Peer | Range | Note |
|---|---|---|
| `@rightkit/react-shell` | `>=0.2.0 <1` | Not optional. `/standard` and the root entry import `@rightkit/react-shell/react`; `/core` does not import it. |
| `@rightkit/license` | `>=0.1.0` | Optional. Needed by `/standard` and the root entry only. No upper bound. |
| `@rightkit/logs` | `>=0.1.0` | Optional. Needed by `/standard` and the root entry only (type use). No upper bound. |
| `@rightkit/theme` | `>=0.2.0 <1` | Optional. Needed by `/standard` and the root entry only. |
| `@rightkit/app-update` | `>=0.2.0` | Optional. Needed by `/standard` and the root entry only (type use). No upper bound. |
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

`@rightkit/settings-ui/core` is the panel shell, `SettingRow`, `SettingsGroup`, search and the backends. It imports `react` and nothing else, so an app that draws its own sections installs none of `@rightkit/license`, `logs`, `theme` or `updates` (they are optional peers). `@rightkit/react-shell` remains a peer for the stylesheet tokens (`settings.css` reads `--rk-*`); `/core` does not import it. The root entry still includes the standard sections, so importing from `@rightkit/settings-ui` needs all peers installed, as in 0.3.x. `src/core-entry.test.ts` walks the import graph of `/core` (source and `dist`) and fails if any of the four appears.

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

If the host draws its own search field and layout, use the context directly (Pulse's `settingsKit.tsx` uses the headless hooks instead; see [Headless search](#headless-search)):

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
| `SettingsGroup` | `hideWhenEmpty` | `false` | Under a provider, hide the group while none of its registered rows or items is visible; re-evaluated on every registration change, so late rows reveal it. Wrap non-row content in `SettingsItem` to count. `"unmount"` (0.4.2) removes the section and its heading instead of hiding them; the rows stay mounted. |
| `SettingsGroup` | `className`, `headingLevel` | none, `3` | (0.4.2) `className` is added after `rk-sg`; `headingLevel` sets the title's heading level. |
| `SettingRow` | `keywords`, `searchText`, `className` | none | Extra terms; text to match when `label` is not a string. `className` (0.4.2) is added after the row's own classes. |
| `SettingRow` | `lead`, `titleClassName`, `hintClassName`, `bareControl` | none, none, none, `false` | (0.4.4) `lead` renders before the text (an icon or badge) in its own column and adds `rk-sr--lead`. `titleClassName` goes on the label and `hintClassName` on the description. `bareControl` renders the control without its `rk-sr__control` wrapper. |
| `SettingsGroup` | `titleClassName`, `hintClassName`, `rowsClassName` | none | (0.4.4) Added after `rk-sg__title`, `rk-sg__desc` (the group's hint) and `rk-sg__rows`. |
| `SettingsSearchProvider` (and the headless hooks) | `enabled` | `true` | (0.4.4) `false` makes search inert: rows, groups, sections, items and hits do not register, do not subscribe to the store, and render as if no provider were there. `SettingsPanel` sets it from its search field: when the panel shows no field (`search={false}`, `hideSearch`, or `hideNav` without `search={true}`) and has no controlled `query`, registrations are skipped. |
| `SettingsSearchSection` | `passthroughWhenInactive` | `false` | (0.4.2) With no search active, render the children untouched: no section, heading or action. |

### Custom properties (0.4.4)

Each defaults to the value shown, so nothing changes unless it is set.

| Property | Default | Applies to |
|---|---|---|
| `--rk-sg-margin` | `22px` | bottom margin of a group |
| `--rk-sg-background` | `var(--rk-panel)` | rows container background |
| `--rk-sg-border-radius` | `var(--rk-radius)` | rows container corner radius |
| `--rk-sg-shadow` | `var(--rk-card)` | rows container shadow |
| `--rk-sr-gap` | `16px` | gap inside a row |
| `--rk-sr-padding` | `12px 16px` | row padding |
| `--rk-sr-border` | `.5px solid var(--rk-hairline)` | row bottom rule |

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

Notes for adoption: matching is "every whitespace-separated term", not the whole trimmed string as one substring; hiding uses `display: none` on groups and sections but unmounts non-matching rows and items; the section action is a `<button>` whose text is yours via `openSectionLabel`.

### Tokens

Set on `:root` or `.rk-settings`. Every default is today's built-in value.

| Token | Default | Controls |
|---|---|---|
| `--rk-settings-padding` | `24px 32px 40px` | Body padding. A value set here applies at every width. Unset, the narrow screen (640px and under) uses `18px 16px 32px`. |
| `--rk-settings-font-size` | `13.5px` | Base font size of the panel |

The panel reads the suite colour tokens from `@rightkit/react-shell/shell.css` (`--rk-plane`, `--rk-chrome`, `--rk-ink` and the rest). It brings no colours of its own.

## Headless search

From 0.4.2, `@rightkit/settings-ui/headless` gives the search state the styled parts compute, as hooks, for a host whose markup and classes are fixed. The hooks render nothing and import no stylesheet; every element is yours. `@rightkit/platform-ui/headless` follows the same pattern.

| Hook or component | Returns or does | Use it for |
|---|---|---|
| `SettingsSearchProvider` | `query`, `filter`, `highlight` (a function for your own `<mark>`), `wrapHighlight` | Wraps the search. Renders no DOM. |
| `useSettingsRow({ id, label, description, keywords, searchText, enabled })` | `{ matches, hidden, props, mark }`. `props` carries `data-setting-id` and `data-hit=""` for a matching row during a search | A row. Spread `props`; wrap label and description in `mark()`; hide it with `hidden`. |
| `useSettingsItem({ id, text, description, keywords })` | The same as `useSettingsRow` | Content that is not a row (an account list, a permission row). It is a result for its group. |
| `useSettingsHit({ text, keywords })` | `{ matches, mark }` | A match that is neither a row nor an item (a command, a help topic). It counts in `count`, `results` and the status sentence, and toward the group or section it sits in. Renders nothing. |
| `useSettingsGroup({ enabled, hideOnlyWhileSearching })` + `SettingsGroupScope` | `{ key, visibleRowCount, empty, hidden, props, mark }` | A group. Pass `key` to the scope around its rows; spread `props` on the group's element; `mark` the title. |
| `useSettingsSection({ enabled, hideOnlyWhileSearching })` + `SettingsSectionScope` | `{ key, visibleRowCount, empty, hidden, props, mark }`; the scope takes `scopeKey={key}` | A section, the same way as a group. |
| `useSettingsSearchField({ query, setQuery })` | `{ value, onChange, clear, active, count, status }` | The search input and its `role="status"` text. Put it under the provider. |
| `useSearchKeys({ inputRef, query, setQuery, findShortcut, escapeClears })` | Cmd/Ctrl+F and Escape policy | Keyboard handling for a host-owned field. |
| `useSettingsSearch()` | `{ query, terms, active, results, count, highlight }` | Anything else that needs the results. |

### Stable tree (use this form)

Keep one element tree and let `hidden` change. A group, section or row whose element stays where it is keeps its children mounted, so a control inside it keeps its typed value and focus when the query changes.

**Swapping wrappers remounts children.** If a host renders the wrapper only while a group is non-empty (or a section only during a search), the rows move between two parents, React unmounts and remounts them, and every control in them loses its local state. The hooks do not prevent this; the stable tree does.

Rules that keep the stable tree correct:

- **Render the children in every case.** A row registers on mount, and an empty group whose rows were not rendered could never reveal itself.
- **A row that does not match returns `null` from the styled parts but `hidden` from the hooks.** To keep a control's state across a query, render the row element with `hidden={r.hidden}` rather than returning `null`. The `hidden` attribute is overridden by a CSS `display` rule on the same element, so add `.ck-set[hidden] { display: none; }` (or set `display` yourself) in the host's stylesheet.
- **Content that is not a row must count.** A group or section that holds plain content and no registered row is `empty`, and is hidden. Wrap such content in `useSettingsItem` or `useSettingsHit`.

This is the shape of Pulse's `settingsKit.tsx`, which uses these hooks (Pulse's `Row` returns `null` when hidden; the `Row` below keeps its element and sets `hidden`):

```tsx
import { Button } from "@rightkit/react-shell/react";
import {
  SettingsGroupScope, SettingsSearchProvider, SettingsSectionScope, useSearchKeys, useSettingsGroup,
  useSettingsRow, useSettingsSearch, useSettingsSearchField, useSettingsSection,
} from "@rightkit/settings-ui/headless";

const mark = (m: string) => <mark className="ck-mark">{m}</mark>;

function Row({ label, note, children }: { label: string; note?: string; children: ReactNode }) {
  const r = useSettingsRow({ label, description: note });
  return (
    <div className="ck-set" {...r.props} hidden={r.hidden}>
      <div className="ck-text">
        <strong>{r.mark(label)}</strong>
        {note && <div className="ck-sub">{r.mark(note)}</div>}
      </div>
      <div className="ck-ctl">{children}</div>
    </div>
  );
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  const g = useSettingsGroup();
  return (
    <SettingsGroupScope groupKey={g.key} title={title}>
      <section className="ck-sgroup" {...g.props}>
        <h2>{g.mark(title)}</h2>
        <div className="ck-card">{children}</div>
      </section>
    </SettingsGroupScope>
  );
}

function Section({ id, title, onOpen, children }: { id: string; title: string; onOpen(id: string): void; children: ReactNode }) {
  const s = useSettingsSection();
  const { active } = useSettingsSearch();
  return (
    <SettingsSectionScope scopeKey={s.key} id={id} title={title}>
      <div className={active ? "ck-search-section" : undefined} {...s.props}>
        {active && (
          <div className="ck-search-head">
            <h3>{s.mark(title)}</h3>
            <Button size="sm" variant="ghost" onClick={() => onOpen(id)}>Open {title}</Button>
          </div>
        )}
        {children}
      </div>
    </SettingsSectionScope>
  );
}

function SearchField({ query, setQuery, inputRef }: { query: string; setQuery(q: string): void; inputRef: RefObject<HTMLInputElement | null> }) {
  const f = useSettingsSearchField({ query, setQuery });
  useSearchKeys({ inputRef, query, setQuery, findShortcut: true, escapeClears: "unless-in-input-or-dialog" });
  return (
    <>
      <input ref={inputRef} className="ck-search" type="search" aria-label="Search settings" value={f.value} onChange={f.onChange} />
      <div role="status" className="ck-sr-only">{f.status}</div>
    </>
  );
}

// In the page, under the provider:
<SettingsSearchProvider query={query.trim()} highlight={mark}>
  <SearchField query={query} setQuery={setQuery} inputRef={input} />
  {sections.map((s) => (
    <Section key={s.id} id={s.id} title={s.title} onOpen={open}>
      <Group title="Startup">
        <Row label="Open at login" note="Start Pulse when you sign in.">{/* control */}</Row>
      </Group>
    </Section>
  ))}
</SettingsSearchProvider>
```

The `Button` is from `@rightkit/react-shell/react`, as Pulse uses it. A host without `@rightkit/react-shell` uses its own `<button>` in the same place.

The `.ck-set[hidden]` rule and the `ck-search-section` class are the host's own CSS. `mark()` wraps every occurrence of every term.

### Highlight wrapper

With a function `highlight`, each matched piece is wrapped in `<span style="display: contents">` and your function's output sits inside it. The span exists so each piece is a keyed element of its own; it has no box and no layout effect, and the text keeps its place. Pass `wrapHighlight={false}` on the provider to render your output as it is, with no span. Either way, text content and accessible names are the same. Use `false` when your CSS targets the direct children of the matched text, or when the output must be the element that the text was.

### Other rules

`hideOnlyWhileSearching` (0.4.4, default `false`): by default a group or section with no registered rows is empty even when no query is active, which is today's behaviour. With `true` it is empty only while a query is active and no row matches, so a group that holds plain content stays visible when you are not searching.

The headless hooks put no element around a label or hint: `mark(label)` returns the text (or the highlighted pieces), so render your own `<b>` for the title and `<small>` for the hint.

A row whose `label` is not a string is not searchable unless you pass `searchText`. In a development build (where `process.env.NODE_ENV` is defined and not `"production"`) the first such row logs one console warning saying so.

Under `search={false}` the hooks are inert: `SettingsPanel` with `search={false}` (or `hideSearch`) and no controlled `query` does not register anything, so the hooks render as if there were no search at all.

Outside a provider every hook is inert: rows render, nothing is hidden, groups and sections are not empty, a hit's `matches` is `true` and `mark` returns the text, and the status is empty. The hooks set no `id`s, no `role`s other than what you ask for, and no `data-` attributes beyond `data-setting-id`, `data-hit`, `data-empty` and `hidden`.

## Known issues

- Three peers have no upper bound: `@rightkit/license`, `@rightkit/logs` and `@rightkit/app-update` (npm audit 2026-10-08, settings-ui finding).
- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:49`).
