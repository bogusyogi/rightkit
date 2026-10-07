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

## 0.2.0 props

- `SettingsSection`: `badge` (nav pill), `heading` (page heading; defaults to `label`), `icon`.
- `standardSections`: `about` optional; `include` / `omit` (built-in ids: `appearance`, `updates`, `models`, `license`, `legal`, `api-keys`, `diagnostics`, `about`); `override` (shallow-merge fields over a built-in by id).
- `updates`: `supportsAutoInstall` (default false; the auto-install toggle is hidden otherwise), `labels` (`Partial<UpdatesLabels>`).
- `license`: `planSlot` (rendered above the licence rows).
- `about`: `extra` (inside the card), `panels` (outside the card).
- `sectionIcons`: the built-in nav icons, reusable in app sections.
