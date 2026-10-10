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
| `@rightkit/settings-ui` | `SettingsPanel`, `SectionFrame`, `mergeSections`, `orderSections`, `searchSections`, `revealSetting`, `SettingsGroup`, `SettingRow`, `ProgressBar`, `formatBytes`, `useSettings`, `standardSections`, `sectionIcons`, `useLicenseSection`, `useDoctor`, and types |
| `@rightkit/settings-ui/settings.css` | Stylesheet |
| `@rightkit/settings-ui/shell-adapter` | `createShellBackends` and the `rightkit-settings` / `rightkit-models` engine wire constants, over the `@rightkit/shell` engine bridge. The engine is described structurally, so this subpath needs no import of `@rightkit/shell`. |

The persistence port is the `SettingsBackend` interface (`load`, `patch` with `expectedRevision`, optional `subscribe`). Use the shell adapter in a Tauri app, or implement the interface over your own store. For example, a blob-backed adapter keeps the whole settings object in one JSON string, bumps a revision on each patch and throws `code: "revision_conflict"` on a stale write. `useSettings(backend)` takes any `SettingsBackend`. `createMemorySettings` is the in-memory reference implementation.

## Peer dependencies

| Peer | Range | Note |
|---|---|---|
| `@rightkit/app-shell` | `>=0.2.0 <1` | |
| `@rightkit/shell` | `>=0.1.0 <1` | Optional. Not imported by the package; `@rightkit/settings-ui/shell-adapter` only needs a structurally compatible `engine`. |
| `@rightkit/license` | `>=0.1.0` | No upper bound |
| `@rightkit/logs` | `>=0.1.0` | No upper bound. Type use only. |
| `@rightkit/theme` | `>=0.2.0 <1` | |
| `@rightkit/updates` | `>=0.2.0` | No upper bound. Type use only. |
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
