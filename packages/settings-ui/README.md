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

## Peer dependencies

| Peer | Range | Note |
|---|---|---|
| `@rightkit/app-shell` | `>=0.2.0 <1` | |
| `@rightkit/shell` | `>=0.1.0 <1` | Type use only |
| `@rightkit/license` | `>=0.1.0` | No upper bound |
| `@rightkit/logs` | `>=0.1.0` | No upper bound. Type use only. |
| `@rightkit/theme` | `>=0.2.0 <1` | |
| `@rightkit/updates` | `>=0.2.0` | No upper bound. Type use only. |
| `react` | `>=18 <20` | |

## Example

Illustrative. `backend` is the app's `SettingsBackend` adapter.

```tsx
import { SettingsPanel, standardSections } from "@rightkit/settings-ui";
import "@rightkit/settings-ui/settings.css";

const sections = standardSections();
<SettingsPanel backend={backend} sections={sections} />;
```

`standardSections` takes `about`, `appearance`, `updates`, `license`, `doctor` and other options. `useLicenseSection(options)` and `useDoctor(checks)` are headless hooks for apps that draw their own markup.

## Known issues

- Three peers have no upper bound: `@rightkit/license`, `@rightkit/logs` and `@rightkit/updates` (npm audit 2026-10-08, settings-ui finding).
- Peer React is `<20`, while `doc-render`, `i18n`, `media-ui` and `pdf-ui` accept `<21`.
