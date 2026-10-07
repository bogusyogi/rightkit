# @rightkit/settings-ui

Settings panel frame and standard sections for RightKit apps. Apps keep their own design through props, slots and `--rk-*` tokens.

```tsx
const sections = mergeSections(
  standardSections({
    updates: { client, currentVersion: "1.0.0", supportsAutoInstall: true },
    about: { appName: "Acme", version: "1.0.0", panels: <Credits /> },
    omit: ["appearance"],
    override: { license: { badge: "FREE", heading: "Your plan" } },
  }),
  [mySection],
);
<SettingsPanel backend={backend} sections={sections} />
```

## 0.2.2 seams

- `SettingsPanel`: `navFooter` (e.g. `<span>Version 1.2.3</span>`), `navHeading` (`ReactNode | false`), `renderNavItem(section, { active })`, `hideNav`. Class/attribute contract for app-drawn active markers: `button.rk-settings__item[data-section-id]` with `aria-current="page"` and `data-active="true"` on the active one only (also `.is-active`).
- `updates.client`: an `AutoUpdateClient` or a function-style adapter `{ check(): Promise<AutoUpdateStatus>; install(): Promise<AutoUpdateStatus>; status?: AutoUpdateStatus | (() => ...) }`. `UpdatesControls` / `useUpdates` are exported to embed the controls inside another card:
  ```tsx
  <UpdatesControls client={adapter} onRestart={restart} labels={{ checkNow: "Check for Updates", installUpdate: "Install and restart", checking: "Checking…", installing: "Installing…" }} />
  ```
  All copy (including the section label, search entry, status sentence and badge) goes through `labels`.
- `license.client`: any `{ activate(key), deactivate(), status() | load() }`; `license.beforeDeactivate` runs before `deactivate()` (e.g. to suppress a bridge).
- `about.card` replaces the card; `about.render({ card, panels, info })` replaces the whole body.

## 0.2.0 props

- `SettingsSection`: `badge` (nav pill), `heading` (page heading; defaults to `label`), `icon`.
- `standardSections`: `about` optional; `include` / `omit` (built-in ids: `appearance`, `updates`, `models`, `license`, `legal`, `api-keys`, `diagnostics`, `about`); `override` (shallow-merge fields over a built-in by id).
- `updates`: `supportsAutoInstall` (default false; the auto-install toggle is hidden otherwise), `labels` (`Partial<UpdatesLabels>`).
- `license`: `planSlot` (rendered above the licence rows).
- `about`: `extra` (inside the card), `panels` (outside the card).
- `sectionIcons`: the built-in nav icons, reusable in app sections.
